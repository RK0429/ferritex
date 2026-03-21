use std::collections::HashMap;

use super::Token;

pub const MAX_REGISTER_INDEX: u16 = 32_767;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterStore {
    counts: HashMap<u16, i32>,
    dimens: HashMap<u16, i32>,
    skips: HashMap<u16, i32>,
    muskips: HashMap<u16, i32>,
    toks: HashMap<u16, Vec<Token>>,
    save_stack: Vec<GroupSave>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GroupSave {
    counts: HashMap<u16, Option<i32>>,
    dimens: HashMap<u16, Option<i32>>,
    skips: HashMap<u16, Option<i32>>,
    muskips: HashMap<u16, Option<i32>>,
    toks: HashMap<u16, Option<Vec<Token>>>,
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
        for (index, previous) in group_save.skips {
            restore_register(&mut self.skips, index, previous);
        }
        for (index, previous) in group_save.muskips {
            restore_register(&mut self.muskips, index, previous);
        }
        for (index, previous) in group_save.toks {
            restore_register(&mut self.toks, index, previous);
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

    pub fn get_skip(&self, index: u16) -> i32 {
        self.skips.get(&index).copied().unwrap_or(0)
    }

    pub fn set_skip(&mut self, index: u16, value: i32, global: bool) {
        self.set_register(index, value, global, RegisterKind::Skip);
    }

    pub fn get_muskip(&self, index: u16) -> i32 {
        self.muskips.get(&index).copied().unwrap_or(0)
    }

    pub fn set_muskip(&mut self, index: u16, value: i32, global: bool) {
        self.set_register(index, value, global, RegisterKind::Muskip);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get_toks(&self, index: u16) -> Vec<Token> {
        self.toks.get(&index).cloned().unwrap_or_default()
    }

    pub fn set_toks(&mut self, index: u16, value: Vec<Token>, global: bool) {
        if global {
            set_sparse_tokens_register(&mut self.toks, index, value);
            for group_save in &mut self.save_stack {
                let _ = group_save.toks.remove(&index);
            }
            return;
        }

        if let Some(group_save) = self.save_stack.last_mut() {
            group_save
                .toks
                .entry(index)
                .or_insert_with(|| self.toks.get(&index).cloned());
        }
        set_sparse_tokens_register(&mut self.toks, index, value);
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
                RegisterKind::Skip => {
                    set_sparse_register(&mut self.skips, index, value);
                    for group_save in &mut self.save_stack {
                        let _ = group_save.skips.remove(&index);
                    }
                }
                RegisterKind::Muskip => {
                    set_sparse_register(&mut self.muskips, index, value);
                    for group_save in &mut self.save_stack {
                        let _ = group_save.muskips.remove(&index);
                    }
                }
                RegisterKind::Toks => unreachable!("token registers use set_toks"),
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
            RegisterKind::Skip => {
                if let Some(group_save) = self.save_stack.last_mut() {
                    group_save
                        .skips
                        .entry(index)
                        .or_insert_with(|| self.skips.get(&index).copied());
                }
                set_sparse_register(&mut self.skips, index, value);
            }
            RegisterKind::Muskip => {
                if let Some(group_save) = self.save_stack.last_mut() {
                    group_save
                        .muskips
                        .entry(index)
                        .or_insert_with(|| self.muskips.get(&index).copied());
                }
                set_sparse_register(&mut self.muskips, index, value);
            }
            RegisterKind::Toks => unreachable!("token registers use set_toks"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum RegisterKind {
    Count,
    Dimen,
    Skip,
    Muskip,
    Toks,
}

fn restore_register<T>(registers: &mut HashMap<u16, T>, index: u16, previous: Option<T>) {
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

fn set_sparse_tokens_register(
    registers: &mut HashMap<u16, Vec<Token>>,
    index: u16,
    value: Vec<Token>,
) {
    if value.is_empty() {
        let _ = registers.remove(&index);
    } else {
        let _ = registers.insert(index, value);
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::{CatCode, Token, TokenKind};

    use super::{RegisterStore, MAX_REGISTER_INDEX};

    #[test]
    fn register_families_default_to_zero_or_empty() {
        let registers = RegisterStore::default();

        assert_eq!(registers.get_count(0), 0);
        assert_eq!(registers.get_dimen(0), 0);
        assert_eq!(registers.get_skip(0), 0);
        assert_eq!(registers.get_muskip(0), 0);
        assert!(registers.get_toks(0).is_empty());
    }

    #[test]
    fn stores_all_register_family_values() {
        let mut registers = RegisterStore::default();
        registers.set_count(12, 34, false);
        registers.set_dimen(56, 78, false);
        registers.set_skip(90, 12, false);
        registers.set_muskip(91, 34, false);
        registers.set_toks(92, vec![char_token('x')], false);

        assert_eq!(registers.get_count(12), 34);
        assert_eq!(registers.get_dimen(56), 78);
        assert_eq!(registers.get_skip(90), 12);
        assert_eq!(registers.get_muskip(91), 34);
        assert_eq!(registers.get_toks(92), vec![char_token('x')]);
    }

    #[test]
    fn group_pop_restores_previous_values() {
        let mut registers = RegisterStore::default();
        registers.set_count(0, 5, false);
        registers.set_dimen(1, 10, false);
        registers.set_skip(2, 15, false);
        registers.set_muskip(3, 20, false);
        registers.set_toks(4, vec![char_token('a')], false);

        registers.push_group();
        registers.set_count(0, 9, false);
        registers.set_dimen(1, 20, false);
        registers.set_skip(2, 25, false);
        registers.set_muskip(3, 30, false);
        registers.set_toks(4, vec![char_token('b')], false);

        assert_eq!(registers.get_count(0), 9);
        assert_eq!(registers.get_dimen(1), 20);
        assert_eq!(registers.get_skip(2), 25);
        assert_eq!(registers.get_muskip(3), 30);
        assert_eq!(registers.get_toks(4), vec![char_token('b')]);

        registers.pop_group();

        assert_eq!(registers.get_count(0), 5);
        assert_eq!(registers.get_dimen(1), 10);
        assert_eq!(registers.get_skip(2), 15);
        assert_eq!(registers.get_muskip(3), 20);
        assert_eq!(registers.get_toks(4), vec![char_token('a')]);
    }

    #[test]
    fn global_assignment_persists_after_group_pop() {
        let mut registers = RegisterStore::default();
        registers.push_group();
        registers.set_count(7, 11, false);
        registers.set_dimen(8, 12, false);
        registers.set_skip(9, 13, false);
        registers.set_muskip(10, 14, false);
        registers.set_toks(11, vec![char_token('l')], false);

        registers.set_count(7, 21, true);
        registers.set_dimen(8, 22, true);
        registers.set_skip(9, 23, true);
        registers.set_muskip(10, 24, true);
        registers.set_toks(11, vec![char_token('g')], true);
        registers.pop_group();

        assert_eq!(registers.get_count(7), 21);
        assert_eq!(registers.get_dimen(8), 22);
        assert_eq!(registers.get_skip(9), 23);
        assert_eq!(registers.get_muskip(10), 24);
        assert_eq!(registers.get_toks(11), vec![char_token('g')]);
    }

    #[test]
    fn supports_boundary_register_indices() {
        let mut registers = RegisterStore::default();
        registers.set_count(0, 1, false);
        registers.set_dimen(MAX_REGISTER_INDEX, 2, false);
        registers.set_skip(MAX_REGISTER_INDEX - 1, 3, false);
        registers.set_muskip(MAX_REGISTER_INDEX - 2, 4, false);
        registers.set_toks(MAX_REGISTER_INDEX - 3, vec![char_token('z')], false);

        assert_eq!(registers.get_count(0), 1);
        assert_eq!(registers.get_dimen(MAX_REGISTER_INDEX), 2);
        assert_eq!(registers.get_skip(MAX_REGISTER_INDEX - 1), 3);
        assert_eq!(registers.get_muskip(MAX_REGISTER_INDEX - 2), 4);
        assert_eq!(
            registers.get_toks(MAX_REGISTER_INDEX - 3),
            vec![char_token('z')]
        );
    }

    fn char_token(char: char) -> Token {
        Token {
            kind: TokenKind::CharToken {
                char,
                cat: CatCode::Other,
            },
            line: 1,
            column: 1,
        }
    }
}
