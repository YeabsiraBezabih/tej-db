use tej_db_core::PageId;

/// Unique identifier for a vector, typically matching the SQL table rowid or primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VectorId(pub u64);

/// Distance metric used for vector similarity computations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistanceMetric {
    #[default]
    L2,
    Cosine,
    DotProduct,
}

/// Constants defining page-aligned Vamana graph constraints.
pub const MAX_DEGREE_R: usize = 64;
pub const DEFAULT_DEGREE_R: usize = 32;
pub const DEFAULT_BEAM_WIDTH: usize = 64;
pub const DEFAULT_ALPHA_PRUNING: f32 = 1.2;

/// Metadata stored inside each 4 KiB `PageType::VectorNode` page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorNodeMetadata {
    pub vector_id: VectorId,
    pub overflow_page: PageId,
    pub alpha: f32,
    pub beta: f32,
    pub norm: f32,
    pub neighbor_count: u16,
    pub quantized_bytes: u16,
    pub is_deleted: bool,
}

/// A candidate neighbor discovered during beam search.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchCandidate {
    pub page_id: PageId,
    pub vector_id: VectorId,
    pub distance: f32,
}

impl Eq for SearchCandidate {}

impl PartialOrd for SearchCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SearchCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Natural ordering by distance: larger distance = larger element.
        // In a BinaryHeap (Max-Heap), .peek()/.pop() will return the farthest candidate.
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}
