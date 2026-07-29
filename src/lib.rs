#![feature(deref_patterns)]

use std::{
    collections::HashMap,
    fs,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};

use crate::{
    analyze::{
        ErrorContext, ErrorVec, Files, Span,
        ast::{AST, FnDef, Item, parse::Parser},
        lex::Lexer,
        semantics,
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
    _marker: PhantomData<(E, A)>,
}

impl<E: Executable, A: Assembler> Compiler<E, A> {
    pub fn compile(
        mut self,
        path: impl Into<PathBuf>,
        out_path: impl AsRef<Path>,
    ) -> Result<(), usize> {
        let path: Rc<PathBuf> = Rc::from(path.into());

        let Ok(code) = self.try_compile(path) else {
            let errors = self.err_ctx.take_errors();
            errors.dump();
            return Err(errors.len());
        };

        E::default()
            .with_binary_identifier("dirthouse")
            .build(code, out_path);

        Ok(())
    }

    fn try_compile(&mut self, filepath: Rc<PathBuf>) -> Result<LinkableCode<A>, ()> {
        let mut ast = AST::new();
        self.import_package(Rc::new(files::stdlib()), &mut ast, (filepath.clone(), 0..1))?;
        let main_package =
            self.import_package(filepath.clone(), &mut ast, (filepath.clone(), 0..1))?;

        let main_fn = format!("{}::main", main_package);
        if !ast
            .items
            .iter()
            .any(|item| matches!(item, Item::Function(FnDef{name,..}) if name == &main_fn))
        {
            self.err_ctx
                .error((filepath.clone(), 0..1))
                .with_message("package must contain a main function at root level")
                .report();

            return Err(());
        }

        match semantics::analyze(ast, &main_fn) {
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

    fn import_package(
        &mut self,
        filepath: Rc<PathBuf>,
        main_ast: &mut AST,
        import_span: Span,
    ) -> Result<String, ()> {
        let Ok(source) = fs::read_to_string(filepath.as_ref()) else {
            self.err_ctx
                .error(import_span.clone())
                .with_message("failed to import package")
                .with_label(import_span, "package imported here")
                .report();

            return Err(());
        };

        let tokens = Lexer::lex(source, filepath.clone(), &mut self.err_ctx)?;
        let mut ast = Parser::parse(tokens, filepath.clone(), &mut self.err_ctx)?;

        let Some(package) = ast.package.clone() else {
            self.err_ctx
                .error((filepath.clone(), 0..1))
                .with_message("main file needs to contain a package statement")
                .report();
            return Err(());
        };

        ast.mangle(&package);
        main_ast.items.extend(ast.items);

        for (submodule, decl_span) in ast.modules {
            let full_mod_path = format!("{}::{}", package, submodule);
            let mod_filepath = filepath
                .as_ref()
                .with_file_name(format!("{}.bl", submodule));
            self.import_module(Rc::new(mod_filepath), &full_mod_path, main_ast, decl_span)?;
        }

        Ok(package)
    }

    fn import_module(
        &mut self,
        filepath: Rc<PathBuf>,
        module: &str,
        main_ast: &mut AST,
        decl_span: Span,
    ) -> Result<(), ()> {
        let Ok(source) = fs::read_to_string(filepath.as_ref()) else {
            self.err_ctx
                .error(decl_span.clone())
                .with_message(format!("failed to locate module {}", module))
                .with_label(decl_span, "module defined here")
                .report();

            return Err(());
        };

        let tokens = Lexer::lex(source, filepath.clone(), &mut self.err_ctx)?;
        let mut ast = Parser::parse(tokens, filepath.clone(), &mut self.err_ctx)?;

        ast.mangle(module);
        main_ast.items.extend(ast.items);

        for (submodule, import_span) in ast.modules {
            let full_mod_path = format!("{}::{}", module, submodule);
            let mod_filepath = filepath
                .as_ref()
                .with_file_name(format!("{}.bl", submodule));
            self.import_module(Rc::new(mod_filepath), &full_mod_path, main_ast, import_span)?;
        }

        Ok(())
    }
}
