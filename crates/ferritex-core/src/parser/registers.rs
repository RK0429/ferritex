use std::collections::HashMap;

pub const MAX_REGISTER_INDEX: u16 = 32_767;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterStore {
    counts: HashMap<u16, i32>,
    dimens: HashMap<u16, i32>,
    save_stack: Vec<GroupSave>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GroupSave {
    counts: HashMap<u16, Option<i32>>,
    dimens: HashMap<u16, Option<i32>>,
}

impl RegisterStore {
    pub fn push_group(&mut self) {
        self.save_stack.push(GroupSave::default());
    }

    pub fn pop_group(&mut self) {
        let Some(group_save) = self.save_stack.pop() else {
            return;
        };

        for (index, previous) in group_save.counts {
            restore_register(&mut self.counts, index, previous);
        }
        for (index, previous) in group_save.dimens {
            restore_register(&mut self.dimens, index, previous);
        }
    }

    pub fn get_count(&self, index: u16) -> i32 {
        self.counts.get(&index).copied().unwrap_or(0)
    }

    pub fn set_count(&mut self, index: u16, value: i32, global: bool) {
        self.set_register(index, value, global, RegisterKind::Count);
    }

    pub fn get_dimen(&self, index: u16) -> i32 {
        self.dimens.get(&index).copied().unwrap_or(0)
    }

    pub fn set_dimen(&mut self, index: u16, value: i32, global: bool) {
        self.set_register(index, value, global, RegisterKind::Dimen);
    }

    fn set_register(&mut self, index: u16, value: i32, global: bool, kind: RegisterKind) {
        if global {
            match kind {
                RegisterKind::Count => {
                    set_sparse_register(&mut self.counts, index, value);
                    for group_save in &mut self.save_stack {
                        let _ = group_save.counts.remove(&index);
                    }
                }
                RegisterKind::Dimen => {
                    set_sparse_register(&mut self.dimens, index, value);
                    for group_save in &mut self.save_stack {
                        let _ = group_save.dimens.remove(&index);
                    }
                }
            }
            return;
        }

        match kind {
            RegisterKind::Count => {
                if let Some(group_save) = self.save_stack.last_mut() {
                    group_save
                        .counts
                        .entry(index)
                        .or_insert_with(|| self.counts.get(&index).copied());
                }
                set_sparse_register(&mut self.counts, index, value);
            }
            RegisterKind::Dimen => {
                if let Some(group_save) = self.save_stack.last_mut() {
                    group_save
                        .dimens
                        .entry(index)
                        .or_insert_with(|| self.dimens.get(&index).copied());
                }
                set_sparse_register(&mut self.dimens, index, value);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterKind {
    Count,
    Dimen,
}

fn restore_register(registers: &mut HashMap<u16, i32>, index: u16, previous: Option<i32>) {
    match previous {
        Some(value) => {
            let _ = registers.insert(index, value);
        }
        None => {
            let _ = registers.remove(&index);
        }
    }
}

fn set_sparse_register(registers: &mut HashMap<u16, i32>, index: u16, value: i32) {
    if value == 0 {
        let _ = registers.remove(&index);
    } else {
        let _ = registers.insert(index, value);
    }
}

#[cfg(test)]
mod tests {
    use super::{RegisterStore, MAX_REGISTER_INDEX};

    #[test]
    fn count_and_dimen_default_to_zero() {
        let registers = RegisterStore::default();

        assert_eq!(registers.get_count(0), 0);
        assert_eq!(registers.get_dimen(0), 0);
    }

    #[test]
    fn stores_count_and_dimen_values() {
        let mut registers = RegisterStore::default();
        registers.set_count(12, 34, false);
        registers.set_dimen(56, 78, false);

        assert_eq!(registers.get_count(12), 34);
        assert_eq!(registers.get_dimen(56), 78);
    }

    #[test]
    fn group_pop_restores_previous_values() {
        let mut registers = RegisterStore::default();
        registers.set_count(0, 5, false);
        registers.set_dimen(1, 10, false);

        registers.push_group();
        registers.set_count(0, 9, false);
        registers.set_dimen(1, 20, false);

        assert_eq!(registers.get_count(0), 9);
        assert_eq!(registers.get_dimen(1), 20);

        registers.pop_group();

        assert_eq!(registers.get_count(0), 5);
        assert_eq!(registers.get_dimen(1), 10);
    }

    #[test]
    fn global_assignment_persists_after_group_pop() {
        let mut registers = RegisterStore::default();
        registers.push_group();
        registers.set_count(7, 11, false);
        registers.set_dimen(8, 12, false);

        registers.set_count(7, 21, true);
        registers.set_dimen(8, 22, true);
        registers.pop_group();

        assert_eq!(registers.get_count(7), 21);
        assert_eq!(registers.get_dimen(8), 22);
    }

    #[test]
    fn supports_boundary_register_indices() {
        let mut registers = RegisterStore::default();
        registers.set_count(0, 1, false);
        registers.set_dimen(MAX_REGISTER_INDEX, 2, false);

        assert_eq!(registers.get_count(0), 1);
        assert_eq!(registers.get_dimen(MAX_REGISTER_INDEX), 2);
    }
}
