use std::collections::HashSet;

use crate::ir::{IR, Item, VirtualReg};

pub fn verify_ssa(ir: &IR) -> Result<(), (String, HashSet<VirtualReg>)> {
    for item in ir.items.iter() {
        let Item::Function {
            name,
            args,
            stack,
            stack_size,
            size_map,
            body,
        } = item;

        let mut duplicate_assignments = HashSet::new();
        let mut assigned_vregs = HashSet::new();
        for op in body.iter().flat_map(|bb| bb.ops.iter()) {
            if let (_, Some(dest)) = op.vregs_used()
                && !assigned_vregs.insert(dest)
            {
                duplicate_assignments.insert(dest);
                println!("dup: {:?}", op);
            }
        }

        if !duplicate_assignments.is_empty() {
            return Err((name.clone().into_owned(), duplicate_assignments));
        }
    }

    Ok(())
}
