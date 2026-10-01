//! ICU's common data, as V8 needs it, filtered to what a game engine ships (see `policy`).
//!
//! V8 takes the bytes once, before its first isolate: `v8::icu::set_common_data_77(icu_data::ICU_DATA)`. They must be
//! 16-byte aligned, which a plain `include_bytes!` is not.

#[cfg(test)]
#[path = "package.rs"]
mod package;
#[cfg(test)]
#[path = "policy.rs"]
mod policy;

#[repr(C, align(16))]
struct Aligned<T: ?Sized>(T);

static DATA: &Aligned<[u8]> = &Aligned(*include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.dat")));

/// The filtered ICU 77 common data.
pub static ICU_DATA: &[u8] = &DATA.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Package;

    fn names(package: &Package<'_>) -> Vec<String> {
        package.items.iter().map(|i| i.name.to_owned()).collect()
    }

    #[test]
    fn the_data_is_aligned_and_a_package() {
        assert_eq!(ICU_DATA.as_ptr() as usize % 16, 0);
        let package = Package::parse(ICU_DATA).expect("the linked data parses");
        assert!(package.items.len() > 1000);
    }

    /// Filtering is dropping items: what is kept is byte for byte what it was, in the order it was.
    #[test]
    fn what_is_kept_is_what_it_was() {
        let full = Package::parse(deno_core_icudata::ICU_DATA).unwrap();
        let ours = Package::parse(ICU_DATA).unwrap();
        let mut remaining = full.items.iter().filter(|i| policy::keep(i.name));
        for item in &ours.items {
            let original = remaining.next().expect("nothing is added");
            assert_eq!(item.name, original.name);
            // The bytes may differ by trailing alignment padding only.
            let (a, b) = (item.bytes, original.bytes);
            let common = a.len().min(b.len());
            assert_eq!(a[..common], b[..common], "{}", item.name);
            assert!(
                a[common..].iter().chain(&b[common..]).all(|x| *x == 0),
                "{}",
                item.name
            );
        }
        assert!(
            remaining.next().is_none(),
            "nothing is lost that the policy keeps"
        );
    }

    /// What is linked is exactly what filtering the full data gives: the build is deterministic and nothing else touched it.
    #[test]
    fn the_linked_data_is_the_filter_applied_to_the_full_data() {
        let full = Package::parse(deno_core_icudata::ICU_DATA).unwrap();
        assert_eq!(full.filtered(policy::keep), ICU_DATA);
    }

    /// Keeping everything gives a package with every item, in order, byte for byte: the rewrite alone changes nothing.
    #[test]
    fn rewriting_without_dropping_keeps_every_item() {
        let full = Package::parse(deno_core_icudata::ICU_DATA).unwrap();
        let again_bytes = full.filtered(|_| true);
        let again = Package::parse(&again_bytes).unwrap();
        assert_eq!(again.items.len(), full.items.len());
        for (a, b) in again.items.iter().zip(&full.items) {
            assert_eq!(a.name, b.name);
            assert_eq!(
                &a.bytes[..b.bytes.len().min(a.bytes.len())],
                &b.bytes[..b.bytes.len().min(a.bytes.len())]
            );
        }
        // Items start where 16-byte alignment says, in the rewritten file too.
        assert_eq!(again_bytes.len() % 16, 0);
    }

    #[test]
    fn the_policy_drops_what_it_says_and_keeps_what_it_must() {
        let full = Package::parse(deno_core_icudata::ICU_DATA).unwrap();
        let prefix = full.items[0].name.split_once('/').unwrap().0;
        let at = |item: &str| format!("{prefix}/{item}");
        for dropped in [
            "gb18030.cnv",
            "cnvalias.icu",
            "translit/root.res",
            "rbnf/root.res",
            "uts46.nrm",
            "lang/af.res",
            "curr/sw.res",
            "zone/zu.res",
            "coll/de.res",
            "coll/ja.res",
        ] {
            // Only the ones this data has: the assertion is about the policy.
            assert!(!policy::keep(&at(dropped)), "{dropped}");
        }
        for kept in [
            "root.res",
            "de.res",
            "en_US.res",
            "zh_Hant_TW.res",
            "pool.res",
            "res_index.res",
            "zoneinfo64.res",
            "metaZones.res",
            "nfkc.nrm",
            "uprops.icu",
            "lang/en.res",
            "lang/zh_Hant.res",
            "curr/de.res",
            "zone/pool.res",
            "coll/root.res",
            "coll/zh.res",
            "coll/ucadata.icu",
            "brkitr/word.brk",
            "brkitr/cjdict.dict",
            "brkitr/thaidict.dict",
            "brkitr/root.res",
        ] {
            assert!(policy::keep(&at(kept)), "{kept}");
        }
    }

    /// The locales the display names and collation were kept for are all really there.
    #[test]
    fn every_kept_locale_has_its_files() {
        let ours = Package::parse(ICU_DATA).unwrap();
        let names = names(&ours);
        let has = |dir: &str, locale: &str| {
            names
                .iter()
                .any(|n| n.contains(&format!("/{dir}/{locale}.res")))
        };
        for locale in policy::KEEP_LOCALES.iter().filter(|l| **l != "root") {
            for dir in ["lang", "region", "curr", "unit", "zone"] {
                assert!(has(dir, locale), "{dir}/{locale}.res is missing");
            }
        }
        assert!(has("coll", "root") && has("coll", "zh"));
        assert!(!has("coll", "de"));
    }

    #[test]
    fn a_package_that_is_not_one_is_refused() {
        assert!(Package::parse(&[]).is_err());
        assert!(Package::parse(&[0x20, 0, 0, 0, 0, 0, 0, 0]).is_err());
        let mut garbage = vec![0u8; 64];
        garbage[0] = 0x10;
        garbage[2] = 0xda;
        garbage[3] = 0x27;
        garbage[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Package::parse(&garbage).is_err());
    }
}
