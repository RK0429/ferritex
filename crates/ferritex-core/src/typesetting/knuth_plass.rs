use crate::kernel::api::DimensionValue;

use super::api::{GlueOrder, HListItem, PENALTY_FORBIDDEN, PENALTY_FORCED};

const MAX_BADNESS: i64 = 1_000_000;

/// Knuth-Plass 行分割のパラメータ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakParams {
    pub line_width: DimensionValue,
    pub tolerance: i32,
    pub line_penalty: i32,
    pub hyphen_penalty: i32,
}

impl Default for BreakParams {
    fn default() -> Self {
        Self {
            line_width: DimensionValue::zero(),
            tolerance: 200,
            line_penalty: 10,
            hyphen_penalty: 50,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateKind {
    Glue,
    Penalty,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BreakCandidate {
    break_index: Option<usize>,
    measure_end: usize,
    next_start: usize,
    penalty: i32,
    kind: CandidateKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Metrics {
    width: i64,
    stretch: i64,
    shrink: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathState {
    cost: i128,
    predecessor: Option<usize>,
}

/// HListItem 列から最適な改行位置のインデックス列を返す
pub fn find_breakpoints(hlist: &[HListItem], params: &BreakParams) -> Vec<usize> {
    if hlist.is_empty() {
        return Vec::new();
    }

    let prefix_metrics = build_prefix_metrics(hlist);
    let mut breakpoints = Vec::new();
    let mut paragraph_start = 0;

    for (index, item) in hlist.iter().enumerate() {
        if matches!(
            item,
            HListItem::Penalty { value } if *value <= PENALTY_FORCED
        ) {
            let paragraph_end = trim_paragraph_end(hlist, paragraph_start, index);
            breakpoints.extend(solve_paragraph(
                hlist,
                &prefix_metrics,
                paragraph_start,
                paragraph_end,
                params,
            ));
            breakpoints.push(index);
            paragraph_start = advance_line_start(hlist, index + 1);
        }
    }

    let paragraph_end = trim_paragraph_end(hlist, paragraph_start, hlist.len());
    breakpoints.extend(solve_paragraph(
        hlist,
        &prefix_metrics,
        paragraph_start,
        paragraph_end,
        params,
    ));

    breakpoints
}

fn solve_paragraph(
    hlist: &[HListItem],
    prefix_metrics: &[Metrics],
    paragraph_start: usize,
    paragraph_end: usize,
    params: &BreakParams,
) -> Vec<usize> {
    if paragraph_start >= paragraph_end {
        return Vec::new();
    }

    let candidates = collect_candidates(hlist, paragraph_start, paragraph_end);
    if candidates.is_empty() {
        return Vec::new();
    }

    compute_best_path(prefix_metrics, &candidates, paragraph_start, params, false)
        .or_else(|| compute_best_path(prefix_metrics, &candidates, paragraph_start, params, true))
        .unwrap_or_default()
}

fn collect_candidates(
    hlist: &[HListItem],
    paragraph_start: usize,
    paragraph_end: usize,
) -> Vec<BreakCandidate> {
    let mut candidates = Vec::new();

    for index in paragraph_start..paragraph_end {
        match hlist[index] {
            HListItem::Glue { .. }
                if index > paragraph_start
                    && matches!(
                        hlist.get(index - 1),
                        Some(HListItem::Char { .. } | HListItem::Kern { .. })
                    ) =>
            {
                candidates.push(BreakCandidate {
                    break_index: Some(index - 1),
                    measure_end: index,
                    next_start: advance_line_start(hlist, index + 1),
                    penalty: 0,
                    kind: CandidateKind::Glue,
                });
            }
            HListItem::Penalty { value } if value < PENALTY_FORBIDDEN && value > PENALTY_FORCED => {
                candidates.push(BreakCandidate {
                    break_index: Some(index),
                    measure_end: index + 1,
                    next_start: advance_line_start(hlist, index + 1),
                    penalty: value,
                    kind: CandidateKind::Penalty,
                });
            }
            _ => {}
        }
    }

    candidates.push(BreakCandidate {
        break_index: None,
        measure_end: paragraph_end,
        next_start: paragraph_end,
        penalty: 0,
        kind: CandidateKind::Terminal,
    });

    candidates
}

fn compute_best_path(
    prefix_metrics: &[Metrics],
    candidates: &[BreakCandidate],
    paragraph_start: usize,
    params: &BreakParams,
    allow_overfull: bool,
) -> Option<Vec<usize>> {
    let mut best_states = vec![None; candidates.len()];

    for candidate_index in 0..candidates.len() {
        if let Some(cost) = edge_cost(
            prefix_metrics,
            paragraph_start,
            candidates[candidate_index],
            params,
            allow_overfull,
        ) {
            best_states[candidate_index] = Some(PathState {
                cost,
                predecessor: None,
            });
        }

        for predecessor_index in 0..candidate_index {
            let Some(previous_state) = best_states[predecessor_index] else {
                continue;
            };

            let start = candidates[predecessor_index].next_start;
            let Some(edge_cost) = edge_cost(
                prefix_metrics,
                start,
                candidates[candidate_index],
                params,
                allow_overfull,
            ) else {
                continue;
            };

            let total_cost = previous_state.cost + edge_cost;
            let should_replace = match best_states[candidate_index] {
                Some(current) => total_cost < current.cost,
                None => true,
            };

            if should_replace {
                best_states[candidate_index] = Some(PathState {
                    cost: total_cost,
                    predecessor: Some(predecessor_index),
                });
            }
        }
    }

    let mut breakpoint_indices = Vec::new();
    let terminal_index = candidates.len().checked_sub(1)?;
    best_states[terminal_index]?;
    let mut current_index = terminal_index;

    while let Some(state) = best_states[current_index] {
        let Some(predecessor_index) = state.predecessor else {
            break;
        };
        if let Some(break_index) = candidates[predecessor_index].break_index {
            breakpoint_indices.push(break_index);
        }
        current_index = predecessor_index;
    }

    breakpoint_indices.reverse();
    Some(breakpoint_indices)
}

fn edge_cost(
    prefix_metrics: &[Metrics],
    line_start: usize,
    candidate: BreakCandidate,
    params: &BreakParams,
    allow_overfull: bool,
) -> Option<i128> {
    if line_start >= candidate.measure_end {
        return None;
    }

    let metrics = metrics_between(prefix_metrics, line_start, candidate.measure_end);
    if metrics.width == 0 {
        return None;
    }

    let line_delta = params.line_width.0 - metrics.width;
    let (feasible, badness) = adjustment_feasibility(line_delta, metrics, candidate.kind, params);
    if !allow_overfull && !feasible {
        return None;
    }

    let badness = i128::from(badness);
    let line_penalty = i128::from(params.line_penalty);
    let penalty = i128::from(candidate.penalty);
    let base = line_penalty + badness;

    Some(if candidate.penalty >= 0 {
        let demerits = base + penalty;
        demerits * demerits
    } else {
        (base * base) - (penalty * penalty)
    })
}

fn adjustment_feasibility(
    line_delta: i64,
    metrics: Metrics,
    candidate_kind: CandidateKind,
    params: &BreakParams,
) -> (bool, i64) {
    if line_delta >= 0 && candidate_kind == CandidateKind::Terminal {
        return (true, 0);
    }

    if line_delta == 0 {
        return (true, 0);
    }

    let (adjustment, delta_abs) = if line_delta > 0 {
        (metrics.stretch, line_delta)
    } else {
        (metrics.shrink, line_delta.saturating_abs())
    };

    if adjustment <= 0 {
        if line_delta > 0 {
            return (false, MAX_BADNESS / 2);
        }
        return (false, MAX_BADNESS);
    }

    let delta_abs = i128::from(delta_abs);
    let adjustment = i128::from(adjustment);
    let badness = (100 * delta_abs.pow(3) / adjustment.pow(3)).min(i128::from(MAX_BADNESS));
    let feasible = badness <= i128::from(params.tolerance.max(0));

    (feasible, badness as i64)
}

fn build_prefix_metrics(hlist: &[HListItem]) -> Vec<Metrics> {
    let mut prefix = Vec::with_capacity(hlist.len() + 1);
    prefix.push(Metrics::default());

    for item in hlist {
        let mut next = prefix[prefix.len() - 1];
        match item {
            HListItem::Char { width, .. } => {
                next.width += width.0;
            }
            HListItem::Kern { width } => {
                next.width += width.0;
            }
            HListItem::Glue {
                width,
                stretch,
                shrink,
                ..
            } => {
                next.width += width.0;
                // Current feasibility math only supports finite glue.
                // Higher-order glue needs separate TeX-style handling, so ignore it here.
                if stretch.order == GlueOrder::Normal {
                    next.stretch += stretch.value.0;
                }
                if shrink.order == GlueOrder::Normal {
                    next.shrink += shrink.value.0;
                }
            }
            HListItem::Penalty { .. } => {}
        }
        prefix.push(next);
    }

    prefix
}

fn metrics_between(prefix_metrics: &[Metrics], start: usize, end: usize) -> Metrics {
    let end_metrics = prefix_metrics[end];
    let start_metrics = prefix_metrics[start];
    Metrics {
        width: end_metrics.width - start_metrics.width,
        stretch: end_metrics.stretch - start_metrics.stretch,
        shrink: end_metrics.shrink - start_metrics.shrink,
    }
}

fn advance_line_start(hlist: &[HListItem], mut index: usize) -> usize {
    while matches!(hlist.get(index), Some(HListItem::Glue { .. })) {
        index += 1;
    }
    index
}

fn trim_paragraph_end(
    hlist: &[HListItem],
    paragraph_start: usize,
    mut paragraph_end: usize,
) -> usize {
    while paragraph_end > paragraph_start {
        match hlist[paragraph_end - 1] {
            HListItem::Glue { .. } => {
                paragraph_end -= 1;
            }
            HListItem::Penalty { value } if value > PENALTY_FORCED => {
                paragraph_end -= 1;
            }
            _ => break,
        }
    }
    paragraph_end
}

#[cfg(test)]
mod tests {
    use super::{find_breakpoints, BreakParams};
    use crate::kernel::api::DimensionValue;
    use crate::typesetting::api::{
        CharWidthProvider, FixedWidthProvider, GlueComponent, HListItem, PENALTY_FORBIDDEN,
        PENALTY_FORCED,
    };

    #[derive(Debug, Clone, Copy)]
    enum TestPart<'a> {
        Word(&'a str),
        Glue,
        Penalty(i32),
    }

    fn dim(value: i64) -> DimensionValue {
        DimensionValue(value)
    }

    fn params(line_width: i64) -> BreakParams {
        BreakParams {
            line_width: dim(line_width),
            ..BreakParams::default()
        }
    }

    fn provider(char_width: i64, space_width: i64) -> FixedWidthProvider {
        FixedWidthProvider {
            char_width: dim(char_width),
            space_width: dim(space_width),
        }
    }

    fn build_hlist(
        provider: FixedWidthProvider,
        stretch: DimensionValue,
        shrink: DimensionValue,
        parts: &[TestPart<'_>],
    ) -> Vec<HListItem> {
        let mut hlist = Vec::new();

        for part in parts {
            match part {
                TestPart::Word(word) => {
                    for codepoint in word.chars() {
                        hlist.push(HListItem::Char {
                            codepoint,
                            width: provider.char_width(codepoint),
                            link: None,
                        });
                    }
                }
                TestPart::Glue => hlist.push(HListItem::Glue {
                    width: provider.space_width(),
                    stretch: GlueComponent::normal(stretch),
                    shrink: GlueComponent::normal(shrink),
                    link: None,
                }),
                TestPart::Penalty(value) => hlist.push(HListItem::Penalty { value: *value }),
            }
        }

        hlist
    }

    #[test]
    fn empty_hlist_has_no_breakpoints() {
        assert_eq!(find_breakpoints(&[], &params(40)), Vec::<usize>::new());
    }

    #[test]
    fn single_word_that_fits_needs_no_break() {
        let hlist = build_hlist(provider(10, 1), dim(5), dim(2), &[TestPart::Word("hello")]);

        assert_eq!(find_breakpoints(&hlist, &params(80)), Vec::<usize>::new());
    }

    #[test]
    fn forced_penalty_always_breaks() {
        let hlist = build_hlist(
            provider(10, 1),
            dim(5),
            dim(2),
            &[
                TestPart::Word("a"),
                TestPart::Penalty(PENALTY_FORCED),
                TestPart::Word("b"),
            ],
        );

        assert_eq!(find_breakpoints(&hlist, &params(100)), vec![1]);
    }

    #[test]
    fn chooses_lower_demerit_path_instead_of_greedy_three_plus_two() {
        let hlist = build_hlist(
            provider(10, 1),
            dim(60),
            dim(1),
            &[
                TestPart::Word("a"),
                TestPart::Glue,
                TestPart::Word("b"),
                TestPart::Glue,
                TestPart::Word("c"),
                TestPart::Penalty(100),
                TestPart::Glue,
                TestPart::Word("d"),
                TestPart::Glue,
                TestPart::Word("e"),
            ],
        );

        assert_eq!(find_breakpoints(&hlist, &params(32)), vec![2]);
    }

    #[test]
    fn emergency_fallback_relaxes_tolerance_when_no_feasible_path_exists() {
        let hlist = build_hlist(
            provider(10, 1),
            dim(10),
            dim(1),
            &[
                TestPart::Word("a"),
                TestPart::Glue,
                TestPart::Word("b"),
                TestPart::Glue,
                TestPart::Word("c"),
            ],
        );
        let params = BreakParams {
            line_width: dim(22),
            tolerance: 0,
            ..BreakParams::default()
        };

        assert_eq!(find_breakpoints(&hlist, &params), vec![2]);
    }

    #[test]
    fn basic_paragraph_wraps_into_three_lines_without_forced_breaks() {
        let hlist = build_hlist(
            provider(10, 1),
            dim(10),
            dim(1),
            &[
                TestPart::Word("a"),
                TestPart::Glue,
                TestPart::Word("b"),
                TestPart::Glue,
                TestPart::Word("c"),
                TestPart::Glue,
                TestPart::Word("d"),
                TestPart::Glue,
                TestPart::Word("e"),
                TestPart::Glue,
                TestPart::Word("f"),
            ],
        );

        assert_eq!(find_breakpoints(&hlist, &params(22)), vec![2, 6]);
    }

    #[test]
    fn forbidden_penalty_is_not_used_as_breakpoint() {
        let hlist = build_hlist(
            provider(10, 1),
            dim(10),
            dim(1),
            &[
                TestPart::Word("a"),
                TestPart::Penalty(PENALTY_FORBIDDEN),
                TestPart::Word("b"),
                TestPart::Glue,
                TestPart::Word("c"),
            ],
        );

        assert_eq!(find_breakpoints(&hlist, &params(20)), vec![2]);
    }

    #[test]
    fn handles_multiple_forced_breaks_as_independent_paragraphs() {
        let hlist = build_hlist(
            provider(10, 1),
            dim(10),
            dim(1),
            &[
                TestPart::Word("a"),
                TestPart::Glue,
                TestPart::Word("b"),
                TestPart::Glue,
                TestPart::Word("c"),
                TestPart::Penalty(PENALTY_FORCED),
                TestPart::Word("d"),
                TestPart::Glue,
                TestPart::Word("e"),
                TestPart::Glue,
                TestPart::Word("f"),
                TestPart::Penalty(PENALTY_FORCED),
            ],
        );

        assert_eq!(find_breakpoints(&hlist, &params(22)), vec![2, 5, 8, 11]);
    }
}
