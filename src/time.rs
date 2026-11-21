//! Conversión entre FILETIME (100 ns desde 1601) y segundos Unix.

pub const FILETIME_UNIX_DIFF: i64 = 11_644_473_600;

pub fn unix_to_filetime(secs: i64) -> u64 {
    let v = secs.saturating_add(FILETIME_UNIX_DIFF);
    (v.max(0) as u64).saturating_mul(10_000_000)
}

pub fn filetime_to_unix(ft: u64) -> i64 {
    (ft / 10_000_000) as i64 - FILETIME_UNIX_DIFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_maps_to_the_known_constant() {
        assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), 0);
    }

    #[test]
    fn roundtrip() {
        for t in [1_600_000_000i64, 1_700_000_000, 0, -86_400] {
            assert_eq!(filetime_to_unix(unix_to_filetime(t)), t);
        }
    }

    #[test]
    fn matches_what_sorteverything_expects() {
        // SortEverything hace ticks/10^7 - 11644473600 (truncando).
        let t = 1_700_000_123i64;
        let ft = unix_to_filetime(t);
        assert_eq!((ft / 10_000_000) as i64 - 11_644_473_600, t);
    }
}
