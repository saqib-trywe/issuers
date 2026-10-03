// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic colours for Tags.
//!
//! Derived Tags have nowhere to store a chosen colour — there is no `tag` row
//! to hang one off — so the name picks its own. Arbitrary, but stable, which
//! is what makes a Tag recognisable at a glance in the list.

use gpui_component::ColorName;

use issuers::domain::Tag;

/// FNV-1a, 64-bit.
///
/// Written out rather than taken from `DefaultHasher`, whose output carries no
/// stability guarantee across Rust releases: a toolchain upgrade must not
/// silently repaint every Tag in the app.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Hashes the case-folded key, so `Bug` and `bug` — one Tag — cannot come out
/// two different colours.
pub(super) fn colour_for(tag: &Tag) -> ColorName {
    let palette = ColorName::all();
    palette[(fnv1a(tag.key().as_bytes()) % palette.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(name: &str) -> Tag {
        name.parse().expect("valid tag")
    }

    #[test]
    fn case_variants_share_a_colour() {
        assert_eq!(colour_for(&tag("Bug")), colour_for(&tag("bug")));
    }

    #[test]
    fn the_same_name_always_lands_on_the_same_colour() {
        assert_eq!(colour_for(&tag("release")), colour_for(&tag("release")));
    }

    #[test]
    fn the_palette_is_actually_spread_over() {
        // Not a distribution proof — just enough to catch a hash that has
        // collapsed to a constant, which would paint every Tag alike.
        let names = ["bug", "ui", "docs", "release", "perf", "flaky", "spike"];
        let distinct: std::collections::HashSet<_> =
            names.iter().map(|name| colour_for(&tag(name))).collect();
        assert!(distinct.len() > 1, "every Tag came out the same colour");
    }
}
