#![feature(deref_patterns)]

use std::{
    cell::{Cell, OnceCell, RefCell, RefMut, UnsafeCell},
    collections::HashMap,
    fmt::{self, Display},
    fs,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};

use ariadne::{Cache, FileCache, Source};

use crate::{
    analyze::{
        ErrorContext, Span,
        ast::{AST, FnDef, Item, parse::Parser},
        lex::Lexer,
        semantics::{self, types::ParsedType},
    },
    ir::IR,
    synthesize::{
        arch::{Assembler, LinkableCode},
        exe::Executable,
    },
};

pub mod analyze;
pub mod files;
pub mod ir;
pub mod synthesize;

#[derive(Default)]
pub struct Compiler<E: Executable, A: Assembler> {
    err_ctx: ErrorContext,
    sources: FileCache,
    _marker: PhantomData<(E, A)>,
}

impl<E: Executable, A: Assembler> Compiler<E, A> {
    pub fn compile(
        mut self,
        path: impl Into<PathBuf>,
        out_path: impl AsRef<Path>,
    ) -> Result<(), usize> {
        let path: Rc<PathBuf> = Rc::from(path.into());

        let mut sources = SourceCache::new();

        let Ok(code) = self.try_compile(path, &sources) else {
            let errors = self.err_ctx.take_errors();
            errors.dump(&mut sources);
            return Err(errors.len());
        };

        E::default()
            .with_binary_identifier("dirthouse")
            .build(code, out_path);

        Ok(())
    }

    fn try_compile(
        &mut self,
        filepath: Rc<PathBuf>,
        sources: &SourceCache,
    ) -> Result<LinkableCode<A>, ()> {
        let mut ast_vec = Vec::new();
        self.import_package(
            sources,
            Rc::new(files::stdlib()),
            &mut ast_vec,
            (filepath.clone(), 0..1),
        )?;
        let main_package = self.import_package(
            sources,
            filepath.clone(),
            &mut ast_vec,
            (filepath.clone(), 0..1),
        )?;

        let (ast, typemap) =
            semantics::types::Resolver::new(&mut self.err_ctx).resolve_and_combine(ast_vec);

        let main_fn = format!("{}::main", main_package);
        if !ast
            .items
            .iter()
            .any(|item| matches!(item, Item::Function(FnDef { name, .. }) if name == &main_fn))
        {
            self.err_ctx
                .error((filepath.clone(), 0..1))
                .with_message("package must contain a main function at root level")
                .report();
        }

        if !self.err_ctx.is_empty() {
            return Err(());
        }

        match semantics::analyze(ast, typemap, &main_fn) {
            Ok((ast, analyzer)) => {
                let ir = IR::generate(ast, &analyzer);
                let code = A::assemble(ir, &main_fn);
                Ok(code)
            }
            Err(errors) => {
                for error in errors.0 {
                    self.err_ctx.report(error);
                }
                Err(())
            }
        }
    }

    fn import_package<'s>(
        &mut self,
        sources: &'s SourceCache,
        filepath: Rc<PathBuf>,
        ast_vec: &mut Vec<AST<'s, (ParsedType<'s>, Span)>>,
        import_span: Span,
    ) -> Result<String, ()> {
        let Ok(source) = sources.get_str(filepath.as_ref()) else {
            self.err_ctx
                .error(import_span.clone())
                .with_message("failed to import package")
                .with_label(import_span, "package imported here")
                .report();

            return Err(());
        };

        let tokens = Lexer::lex(source, filepath.clone(), &mut self.err_ctx)?;
        let mut ast = Parser::parse(tokens, source, filepath.clone(), &mut self.err_ctx)?;

        let Some(package) = ast.package else {
            self.err_ctx
                .error((filepath.clone(), 0..1))
                .with_message("main file needs to contain a package statement")
                .report();
            return Err(());
        };

        for (submodule, decl_span) in ast.modules.iter() {
            let full_mod_path = format!("{}::{}", package, submodule);
            let mod_filepath = filepath
                .as_ref()
                .with_file_name(format!("{}.bl", submodule));

            self.import_module(
                sources,
                Rc::new(mod_filepath),
                &full_mod_path,
                ast_vec,
                decl_span.clone(),
            )?;
        }

        ast.mangle(package);
        ast_vec.push(ast);

        Ok(package.to_owned())
    }

    fn import_module<'s>(
        &mut self,
        sources: &'s SourceCache,
        filepath: Rc<PathBuf>,
        module: &str,
        ast_vec: &mut Vec<AST<'s, (ParsedType<'s>, Span)>>,
        decl_span: Span,
    ) -> Result<(), ()> {
        let Ok(source) = sources.get_str(filepath.as_ref()) else {
            self.err_ctx
                .error(decl_span.clone())
                .with_message(format!("failed to locate module {}", module))
                .with_label(decl_span, "module defined here")
                .report();

            return Err(());
        };

        let tokens = Lexer::lex(source, filepath.clone(), &mut self.err_ctx)?;
        let mut ast: AST<'s, (ParsedType<'s>, Span)> =
            Parser::parse(tokens, source, filepath.clone(), &mut self.err_ctx)?;

        for (submodule, import_span) in ast.modules.iter() {
            let full_mod_path = format!("{}::{}", module, submodule);
            let mod_filepath = filepath
                .as_ref()
                .with_file_name(format!("{}.bl", submodule));

            self.import_module(
                sources,
                Rc::new(mod_filepath),
                &full_mod_path,
                ast_vec,
                import_span.clone(),
            )?;
        }

        ast.mangle(module);
        ast_vec.push(ast);

        Ok(())
    }
}

struct SourceCache {
    files: RefCell<HashMap<PathBuf, Source>>,
}

impl SourceCache {
    pub fn new() -> Self {
        Self {
            files: RefCell::new(HashMap::new()),
        }
    }

    pub fn get_str(&self, path: &Path) -> Result<&str, std::io::Error> {
        self.get(path).map(|s| s.text())
    }

    pub fn get(&self, path: &Path) -> Result<&Source, std::io::Error> {
        let mut files = self.files.borrow_mut();

        if !files.contains_key(path) {
            let text = fs::read_to_string(path)?;
            files.insert(path.to_owned(), Source::from(text));
        }

        // Since the returned value and self is borrowed for lifetime 's
        // and the source text is never moved, this should be safe...
        Ok(unsafe { std::mem::transmute::<&Source, &Source>(files.get(path).unwrap()) })
    }
}

impl ariadne::Cache<Rc<PathBuf>> for SourceCache {
    type Storage = String;

    fn fetch(&mut self, path: &Rc<PathBuf>) -> Result<&Source<Self::Storage>, impl fmt::Debug> {
        self.get(path)
    }

    fn display<'a>(&self, path: &'a Rc<PathBuf>) -> Option<impl Display + 'a> {
        path.to_str()
    }
}
