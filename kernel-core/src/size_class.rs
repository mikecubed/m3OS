//! Size-class table for the slab allocator.
//!
//! Maps arbitrary allocation sizes (1..=4096) to a fixed set of 13 slab
//! size classes. The table follows roughly 2 steps per doubling up to 1024,
//! then uses full doublings for the large buckets.

/// The 13 size classes (bytes), in ascending order.
///
/// This exact table is the Phase 53a contract:
/// 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 2048, 4096.
///
/// With these exact buckets the geometric region (32..=1024) stays below
/// ~34 % internal waste, while the 2048→4096 jump reaches ~50 % worst-case
/// waste for requests just above 2048.
pub const SIZE_CLASSES: [usize; 13] = [
    32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 2048, 4096,
];

/// Number of size classes.
pub const NUM_SIZE_CLASSES: usize = SIZE_CLASSES.len();

/// Return the index of the smallest size class that can satisfy `size`,
/// or `None` when `size` is 0 or exceeds the largest class (4096).
///
/// This is a compile-time-driven lookup: the class table is a constant
/// array and the search is a simple linear scan over 13 entries.
#[inline]
pub fn size_to_class(size: usize) -> Option<usize> {
    if size == 0 {
        return None;
    }
    // Linear scan — 13 entries is small enough that a branch-free loop
    // beats a binary search on all modern CPUs.
    let mut i = 0;
    while i < SIZE_CLASSES.len() {
        if SIZE_CLASSES[i] >= size {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Return the size-class bucket size for a given allocation size, or
/// `None` when `size` is 0 or exceeds the largest class.
#[inline]
pub fn class_size_for(size: usize) -> Option<usize> {
    size_to_class(size).map(|idx| SIZE_CLASSES[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_classes_are_sorted_and_correct() {
        assert_eq!(
            SIZE_CLASSES,
            [
                32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 2048, 4096
            ]
        );
        for w in SIZE_CLASSES.windows(2) {
            assert!(w[0] < w[1], "classes must be strictly ascending");
        }
    }

    #[test]
    fn num_size_classes_matches() {
        assert_eq!(NUM_SIZE_CLASSES, 13);
    }

    #[test]
    fn zero_returns_none() {
        assert_eq!(size_to_class(0), None);
    }

    #[test]
    fn over_4096_returns_none() {
        assert_eq!(size_to_class(4097), None);
        assert_eq!(size_to_class(8192), None);
        assert_eq!(size_to_class(usize::MAX), None);
    }

    /// Every size 1..=4096 maps to the smallest class that can hold it.
    #[test]
    fn every_size_maps_to_smallest_fitting_class() {
        for size in 1..=4096usize {
            let idx = size_to_class(size)
                .unwrap_or_else(|| panic!("size {} should map to a class", size));
            let cls = SIZE_CLASSES[idx];

            // The selected class must actually fit the request.
            assert!(
                cls >= size,
                "class {} (idx {}) is too small for size {}",
                cls,
                idx,
                size
            );

            // No smaller class should also fit.
            if idx > 0 {
                let prev = SIZE_CLASSES[idx - 1];
                assert!(
                    prev < size,
                    "size {} mapped to class {} (idx {}) but class {} (idx {}) also fits",
                    size,
                    cls,
                    idx,
                    prev,
                    idx - 1
                );
            }
        }
    }

    /// Internal waste is bounded for every size in 1..=4096.
    ///
    /// These 13 classes use ≈2 steps per doubling up to 1024, then full
    /// doublings (1024→2048→4096).  The theoretical worst-case waste is
    /// just under 50 % (at size 2049 → class 4096).  For the sub-1024
    /// range (2 steps per doubling) the bound is ≈33 %.
    ///
    /// Sizes below the minimum class (32) have higher waste because 32 is
    /// the smallest usable object size (freelist pointer + alignment).
    #[test]
    fn internal_waste_bounded() {
        let min_class = SIZE_CLASSES[0];
        let mut max_waste_pct: f64 = 0.0;
        let mut worst_size: usize = 0;

        // Below the minimum class, waste is inherently high — just verify
        // they map to class 0.
        for size in 1..min_class {
            assert_eq!(size_to_class(size), Some(0));
        }

        // From the minimum class onward, the geometric progression keeps
        // waste below 50 %.
        for size in min_class..=4096usize {
            let idx = size_to_class(size).unwrap();
            let cls = SIZE_CLASSES[idx] as f64;
            let waste_pct = (cls - size as f64) / cls * 100.0;
            if waste_pct > max_waste_pct {
                max_waste_pct = waste_pct;
                worst_size = size;
            }
            assert!(
                waste_pct < 50.0,
                "size {} -> class {} has {:.2}% waste (>= 50%)",
                size,
                SIZE_CLASSES[idx],
                waste_pct,
            );
        }

        // Within the geometric range (32..=1024) waste is ≤ 33.3 %.
        for size in min_class..=1024usize {
            let idx = size_to_class(size).unwrap();
            let cls = SIZE_CLASSES[idx] as f64;
            let waste_pct = (cls - size as f64) / cls * 100.0;
            assert!(
                waste_pct < 34.0,
                "size {} -> class {} has {:.2}% waste (>= 34% in geometric range)",
                size,
                SIZE_CLASSES[idx],
                waste_pct,
            );
        }

        // Informational: print worst-case for review.
        #[cfg(feature = "std")]
        eprintln!(
            "worst-case waste: {:.2}% at size {} -> class {}",
            max_waste_pct,
            worst_size,
            SIZE_CLASSES[size_to_class(worst_size).unwrap()]
        );
        let _ = (max_waste_pct, worst_size);
    }

    #[test]
    fn exact_class_boundaries() {
        for (i, &cls) in SIZE_CLASSES.iter().enumerate() {
            assert_eq!(
                size_to_class(cls),
                Some(i),
                "exact class size {} should map to index {}",
                cls,
                i
            );
        }
    }

    #[test]
    fn class_size_for_roundtrip() {
        assert_eq!(class_size_for(0), None);
        assert_eq!(class_size_for(1), Some(32));
        assert_eq!(class_size_for(32), Some(32));
        assert_eq!(class_size_for(33), Some(48));
        assert_eq!(class_size_for(4096), Some(4096));
        assert_eq!(class_size_for(4097), None);
    }
}
