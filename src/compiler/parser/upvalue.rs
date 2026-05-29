use super::Parser;
use super::UpvalueDesc;
use super::find_last_local;

impl Parser<'_> {
    /// Find an existing upvalue by name.
    pub(super) fn find_upvalue(&self, name: &str) -> Option<u8> {
        self.upvalues
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| i as u8)
    }

    /// Try to resolve a variable as an upvalue from outer scopes.
    /// Returns the upvalue index if found and added.
    pub(super) fn resolve_upvalue(&mut self, name: &str) -> Option<u8> {
        self.resolve_upvalue_recursive(name, self.outer_locals.len())
    }

    /// Recursively resolve an upvalue, creating upvalues in intermediate scopes as needed.
    /// `depth` is how many levels of outer scopes to check (starting from the current function).
    fn resolve_upvalue_recursive(&mut self, name: &str, depth: usize) -> Option<u8> {
        if depth == 0 {
            return None;
        }

        let parent_idx = depth - 1;

        // Check the parent's locals
        let parent_locals = &self.outer_locals[parent_idx];
        if let Some(local_idx) = find_last_local(parent_locals, name) {
            // Found in parent's locals - capture as Local
            let idx = self.add_upvalue(name, UpvalueDesc::Local(local_idx as u8));
            return Some(idx);
        }

        // Check the parent's existing upvalues
        let parent_upvalues = &self.outer_upvalues[parent_idx];
        if let Some(upvalue_idx) = parent_upvalues.iter().position(|(n, _)| n == name) {
            // Found in parent's upvalues - capture as Upvalue
            let idx = self.add_upvalue(name, UpvalueDesc::Upvalue(upvalue_idx as u8));
            return Some(idx);
        }

        // Not found in parent - try to recursively resolve from grandparent
        // First, we need to make the parent capture this variable as an upvalue
        if parent_idx > 0 {
            // Temporarily work with the parent's scope to create its upvalue
            // We need to check if the variable exists further up
            if let Some(parent_upvalue_idx) = self.create_parent_upvalue(name, parent_idx) {
                // Now the parent has this as an upvalue, so we can capture it
                let idx = self.add_upvalue(name, UpvalueDesc::Upvalue(parent_upvalue_idx));
                return Some(idx);
            }
        }

        None
    }

    /// Create an upvalue in the parent scope for a variable from an even outer scope.
    /// Returns the upvalue index in the parent's upvalue list.
    fn create_parent_upvalue(&mut self, name: &str, parent_idx: usize) -> Option<u8> {
        // Check grandparent's locals
        if parent_idx > 0 {
            let grandparent_idx = parent_idx - 1;
            let grandparent_locals = &self.outer_locals[grandparent_idx];
            if let Some(local_idx) = find_last_local(grandparent_locals, name) {
                // Found in grandparent's locals - parent captures as Local
                let upvalue_idx = self.outer_upvalues[parent_idx].len() as u8;
                self.outer_upvalues[parent_idx]
                    .push((name.to_string(), UpvalueDesc::Local(local_idx as u8)));
                return Some(upvalue_idx);
            }

            // Check grandparent's upvalues
            let grandparent_upvalues = &self.outer_upvalues[grandparent_idx];
            if let Some(gp_upvalue_idx) = grandparent_upvalues.iter().position(|(n, _)| n == name) {
                // Found in grandparent's upvalues - parent captures as Upvalue
                let upvalue_idx = self.outer_upvalues[parent_idx].len() as u8;
                self.outer_upvalues[parent_idx]
                    .push((name.to_string(), UpvalueDesc::Upvalue(gp_upvalue_idx as u8)));
                return Some(upvalue_idx);
            }

            // Recurse further up if needed
            if grandparent_idx > 0
                && let Some(gp_upvalue_idx) = self.create_parent_upvalue(name, grandparent_idx)
            {
                // Grandparent now has this as an upvalue, so parent can capture it
                let upvalue_idx = self.outer_upvalues[parent_idx].len() as u8;
                self.outer_upvalues[parent_idx]
                    .push((name.to_string(), UpvalueDesc::Upvalue(gp_upvalue_idx)));
                return Some(upvalue_idx);
            }
        }

        None
    }

    /// Add a new upvalue and return its index.
    fn add_upvalue(&mut self, name: &str, desc: UpvalueDesc) -> u8 {
        let idx = self.upvalues.len() as u8;
        self.upvalues.push((name.to_string(), desc));
        idx
    }
}
