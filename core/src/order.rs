//! Stable sorting by an integer rank, through one shared sort.
//!
//! Every `sort_by_key` call site compiles a sort of its own, a few
//! kilobytes each, which is weight a browser downloads. Sorting by a rank
//! instead sorts the `(rank, position)` pairs with the one sort below and
//! moves the items into that order, so every caller shares a single copy.

/// Sort `items` by `rank`, keeping items of equal rank in their order.
pub fn sort_by_rank<T>(items: &mut [T], mut rank: impl FnMut(&T) -> u64) {
    if items.len() < 2 {
        return;
    }
    let mut keys: Vec<(u64, u32)> = items
        .iter()
        .enumerate()
        .map(|(i, item)| (rank(item), i as u32))
        .collect();
    sort_keys(&mut keys);
    // `keys[i].1` is where the item that belongs at `i` sits now. Each swap
    // puts one item in place; one already moved is found by following the
    // chain of earlier swaps.
    for i in 0..keys.len() {
        let mut from = keys[i].1 as usize;
        while from < i {
            from = keys[from].1 as usize;
        }
        items.swap(i, from);
    }
}

/// The one sort every [`sort_by_rank`] shares. The position breaks ties, so
/// an unstable sort keeps equal ranks in order.
#[inline(never)]
fn sort_keys(keys: &mut [(u64, u32)]) {
    keys.sort_unstable();
}

/// A rank that orders floats as `<` does, with every NaN after them.
pub fn float_rank(x: f32) -> u64 {
    let bits = x.to_bits();
    u64::from(if bits >> 31 == 1 {
        !bits
    } else {
        bits | 1 << 31
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_ranks_keep_their_order() {
        let mut v = vec![(3, 'a'), (1, 'b'), (3, 'c'), (2, 'd'), (1, 'e'), (0, 'f')];
        let mut want = v.clone();
        want.sort_by_key(|p| p.0);
        sort_by_rank(&mut v, |p| p.0 as u64);
        assert_eq!(v, want);
    }

    #[test]
    fn every_permutation_of_five_sorts() {
        let mut v: Vec<u32> = (0..5).collect();
        // Heap's algorithm over every order of five items.
        fn each(v: &mut Vec<u32>, k: usize, seen: &mut usize) {
            if k == 1 {
                let mut s = v.clone();
                sort_by_rank(&mut s, |&x| u64::from(x));
                assert_eq!(s, [0, 1, 2, 3, 4]);
                *seen += 1;
                return;
            }
            for i in 0..k {
                each(v, k - 1, seen);
                let j = if k.is_multiple_of(2) { i } else { 0 };
                v.swap(j, k - 1);
            }
        }
        let mut seen = 0;
        each(&mut v, 5, &mut seen);
        assert_eq!(seen, 120);
    }

    #[test]
    fn floats_rank_in_order() {
        let xs = [-2.5f32, -0.0, 0.0, 0.25, 1.0, 7.0];
        for w in xs.windows(2) {
            assert!(float_rank(w[0]) <= float_rank(w[1]), "{w:?}");
        }
    }
}
