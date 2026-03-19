#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecompilationScope {
    FullDocument,
    LocalRegion,
}
