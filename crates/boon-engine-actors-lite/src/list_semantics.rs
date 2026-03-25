pub(crate) fn get_one_based<T>(items: &[T], one_based_index: i64) -> Option<&T> {
    if one_based_index < 1 {
        None
    } else {
        items.get((one_based_index - 1) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::get_one_based;

    #[test]
    fn one_based_lookup_returns_expected_items() {
        let values = vec!["first", "second", "third"];

        assert_eq!(get_one_based(&values, 0), None);
        assert_eq!(get_one_based(&values, 1), Some(&"first"));
        assert_eq!(get_one_based(&values, 3), Some(&"third"));
        assert_eq!(get_one_based(&values, 4), None);
    }
}
