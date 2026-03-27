#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StageOrder {
    MacroSessionDelta,
    DocumentReferenceBibliography,
    LayoutPageNumberMerge,
    ArtifactEmissionCacheMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageCommitPayload<T> {
    pub stage_order: StageOrder,
    pub partition_id: String,
    pub payload: T,
}

impl<T> StageCommitPayload<T> {
    pub fn new(stage_order: StageOrder, partition_id: impl Into<String>, payload: T) -> Self {
        Self {
            stage_order,
            partition_id: partition_id.into(),
            payload,
        }
    }

    pub fn layout_merge(partition_id: impl Into<String>, payload: T) -> Self {
        Self::new(StageOrder::LayoutPageNumberMerge, partition_id, payload)
    }
}

#[derive(Debug, Clone)]
pub struct CommitBarrier<T> {
    pass_number: u32,
    pending: Vec<StageCommitPayload<T>>,
}

impl<T> CommitBarrier<T> {
    pub fn new(pass_number: u32) -> Self {
        Self {
            pass_number,
            pending: Vec::new(),
        }
    }

    pub fn pass_number(&self) -> u32 {
        self.pass_number
    }

    pub fn commit(&mut self, payload: StageCommitPayload<T>) {
        self.pending.push(payload);
    }

    pub fn into_ordered(mut self) -> Vec<StageCommitPayload<T>> {
        self.pending.sort_by(|left, right| {
            left.stage_order
                .cmp(&right.stage_order)
                .then_with(|| left.partition_id.cmp(&right.partition_id))
        });
        self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::{CommitBarrier, StageCommitPayload, StageOrder};

    #[test]
    fn commit_barrier_orders_payloads_by_stage_then_partition() {
        let mut barrier = CommitBarrier::new(2);
        barrier.commit(StageCommitPayload::layout_merge("section:0002", "layout-b"));
        barrier.commit(StageCommitPayload::new(
            StageOrder::DocumentReferenceBibliography,
            "section:0003",
            "refs",
        ));
        barrier.commit(StageCommitPayload::layout_merge("section:0001", "layout-a"));

        let ordered = barrier
            .into_ordered()
            .into_iter()
            .map(|payload| payload.payload)
            .collect::<Vec<_>>();

        assert_eq!(ordered, vec!["refs", "layout-a", "layout-b"]);
    }

    #[test]
    fn commit_barrier_preserves_pass_number() {
        let barrier = CommitBarrier::<()>::new(3);

        assert_eq!(barrier.pass_number(), 3);
    }
}
