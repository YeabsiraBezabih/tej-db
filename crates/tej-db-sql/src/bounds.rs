use std::ops::Bound;

pub(crate) fn max_start_bound(b1: Bound<Vec<u8>>, b2: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
    match (b1, b2) {
        (Bound::Unbounded, b) | (b, Bound::Unbounded) => b,
        (Bound::Included(k1), Bound::Included(k2)) => {
            if k1 >= k2 {
                Bound::Included(k1)
            } else {
                Bound::Included(k2)
            }
        }
        (Bound::Excluded(k1), Bound::Excluded(k2)) => {
            if k1 >= k2 {
                Bound::Excluded(k1)
            } else {
                Bound::Excluded(k2)
            }
        }
        (Bound::Included(k1), Bound::Excluded(k2)) => {
            if k1 > k2 {
                Bound::Included(k1)
            } else {
                Bound::Excluded(k2)
            }
        }
        (Bound::Excluded(k1), Bound::Included(k2)) => {
            if k1 >= k2 {
                Bound::Excluded(k1)
            } else {
                Bound::Included(k2)
            }
        }
    }
}

pub(crate) fn min_end_bound(b1: Bound<Vec<u8>>, b2: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
    match (b1, b2) {
        (Bound::Unbounded, b) | (b, Bound::Unbounded) => b,
        (Bound::Included(k1), Bound::Included(k2)) => {
            if k1 <= k2 {
                Bound::Included(k1)
            } else {
                Bound::Included(k2)
            }
        }
        (Bound::Excluded(k1), Bound::Excluded(k2)) => {
            if k1 <= k2 {
                Bound::Excluded(k1)
            } else {
                Bound::Excluded(k2)
            }
        }
        (Bound::Included(k1), Bound::Excluded(k2)) => {
            if k1 < k2 {
                Bound::Included(k1)
            } else {
                Bound::Excluded(k2)
            }
        }
        (Bound::Excluded(k1), Bound::Included(k2)) => {
            if k1 <= k2 {
                Bound::Excluded(k1)
            } else {
                Bound::Included(k2)
            }
        }
    }
}
