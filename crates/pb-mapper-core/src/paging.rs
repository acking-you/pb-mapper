//! One page out of a fully materialised listing.
//!
//! The relay answers every paginated administrator request by collecting and
//! sorting its whole table and then slicing one page out of it. Both the routing
//! runtime and the credential actor do that, and the SDK has to agree with them
//! on the page-size ceiling, so the slice and the ceiling live here — the bottom
//! layer all three can reach.

/// Largest page any listing will serve.
///
/// A protocol constant: the relay clamps to it, and the SDK rejects an oversized
/// request against it before sending. Both read it from here so the two sides
/// cannot drift apart silently.
pub const MAX_PAGE_SIZE: u16 = 1000;

/// Slice one page out of `all`, returning the items and the next page's index.
///
/// `page_size` is clamped into `1..=MAX_PAGE_SIZE`, so a caller asking for zero
/// still makes progress rather than getting an empty page forever. A `page` past
/// the end yields no items and no cursor.
///
/// Sorting belongs to the caller: every listing orders by different fields, and
/// the order is part of what that listing means.
pub fn paginate<T: Clone>(all: &[T], page: u32, page_size: u16) -> (Vec<T>, Option<u32>) {
    let page_size = page_size.clamp(1, MAX_PAGE_SIZE) as usize;
    let start = (page as usize).saturating_mul(page_size);
    let items = all.iter().skip(start).take(page_size).cloned().collect();
    let next_page = (start.saturating_add(page_size) < all.len()).then_some(page.saturating_add(1));
    (items, next_page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_listing_has_no_items_and_no_next_page() {
        let (items, next_page) = paginate::<u8>(&[], 0, 10);
        assert!(items.is_empty());
        assert_eq!(next_page, None);
    }

    #[test]
    fn the_last_page_reports_no_next_page() {
        let all = [1, 2, 3, 4, 5];
        let (items, next_page) = paginate(&all, 0, 3);
        assert_eq!(items, vec![1, 2, 3]);
        assert_eq!(next_page, Some(1));

        let (items, next_page) = paginate(&all, 1, 3);
        assert_eq!(items, vec![4, 5]);
        assert_eq!(next_page, None);
    }

    /// A full final page still ends the listing: the cursor is only offered when
    /// there is something past it.
    #[test]
    fn an_exactly_full_final_page_ends_the_listing() {
        let (items, next_page) = paginate(&[1, 2, 3, 4], 1, 2);
        assert_eq!(items, vec![3, 4]);
        assert_eq!(next_page, None);
    }

    #[test]
    fn a_zero_page_size_is_clamped_to_one() {
        let (items, next_page) = paginate(&[1, 2, 3], 0, 0);
        assert_eq!(items, vec![1]);
        assert_eq!(next_page, Some(1));
    }

    #[test]
    fn an_oversized_page_size_is_clamped_to_the_maximum() {
        let all: Vec<u32> = (0..MAX_PAGE_SIZE as u32 + 5).collect();
        let (items, next_page) = paginate(&all, 0, u16::MAX);
        assert_eq!(items.len(), MAX_PAGE_SIZE as usize);
        assert_eq!(next_page, Some(1));
    }

    #[test]
    fn a_page_past_the_end_is_empty() {
        let (items, next_page) = paginate(&[1, 2, 3], 99, 10);
        assert!(items.is_empty());
        assert_eq!(next_page, None);
    }
}
