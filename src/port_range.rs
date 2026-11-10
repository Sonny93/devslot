//! A block of consecutive ports.

use std::ops::RangeInclusive;

/// The ports from `first` to `last`, both included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    /// The lowest port.
    pub first: u16,
    /// The highest port.
    pub last: u16,
}

impl PortRange {
    /// How many ports the range holds; zero when it is reversed.
    pub fn len(self) -> usize {
        if self.first > self.last {
            return 0;
        }
        usize::from(self.last - self.first) + 1
    }

    /// Whether the range holds no port.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Whether `port` is in the range.
    pub fn contains(self, port: u16) -> bool {
        (self.first..=self.last).contains(&port)
    }

    /// Whether the two ranges share at least one port.
    pub fn overlaps(self, other: Self) -> bool {
        self.first <= other.last && other.first <= self.last
    }

    /// Every port, lowest first.
    pub fn ports(self) -> RangeInclusive<u16> {
        self.first..=self.last
    }
}

impl std::fmt::Display for PortRange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}-{}", self.first, self.last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn range(first: u16, last: u16) -> PortRange {
        PortRange { first, last }
    }

    #[test]
    fn should_count_both_ends_when_measuring_the_length() {
        assert_eq!(range(20050, 20099).len(), 50);
    }

    #[test]
    fn should_have_no_length_when_the_range_is_reversed() {
        assert!(range(20099, 20050).is_empty());
    }

    #[test]
    fn should_overlap_when_the_ranges_share_their_boundary_port() {
        assert!(range(10, 20).overlaps(range(20, 30)));
    }

    #[test]
    fn should_not_overlap_when_the_ranges_are_adjacent() {
        assert!(!range(10, 19).overlaps(range(20, 30)));
    }

    #[test]
    fn should_contain_the_boundary_ports() {
        let block = range(10, 20);

        assert!(block.contains(10) && block.contains(20) && !block.contains(21));
    }

    #[test]
    fn should_display_as_first_dash_last() {
        assert_eq!(range(20050, 20099).to_string(), "20050-20099");
    }
}
