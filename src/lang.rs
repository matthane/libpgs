//! Language code normalization to BCP 47 (ISO 639-1 when available, ISO 639-2/T otherwise).
//!
//! Both MKV and M2TS containers store language codes in various formats:
//! - MKV `Language` element: ISO 639-2/B (bibliographic), e.g. "chi", "fre"
//! - MKV `LanguageBCP47` element: BCP 47, e.g. "zh", "fr", "zh-Hans"
//! - M2TS PMT descriptors: ISO 639-2, e.g. "zho", "fra"
//! - BDMV CLPI files: ISO 639-2, e.g. "zho", "fra"
//!
//! This module normalizes all codes to the shortest standard form:
//! ISO 639-1 (2-letter) when one exists, otherwise ISO 639-2/T (3-letter).

/// Normalize a language code to its shortest standard form.
///
/// - 3-letter ISO 639-2/B or /T codes are mapped to 2-letter ISO 639-1 where available.
/// - 2-letter codes and BCP 47 tags (e.g. "zh-Hans") are passed through unchanged.
/// - Unknown codes are passed through unchanged.
pub(crate) fn normalize_language(code: &str) -> String {
    // BCP 47 subtags (e.g. "zh-Hans") — normalize just the primary subtag.
    if let Some(dash) = code.find('-') {
        let primary = &code[..dash];
        let rest = &code[dash..];
        let normalized = normalize_primary(primary);
        return format!("{normalized}{rest}");
    }

    normalize_primary(code)
}

/// Normalize a primary language subtag (no region/script suffixes).
fn normalize_primary(code: &str) -> String {
    let lower = code.to_ascii_lowercase();

    if lower.len() == 3 {
        if let Some(two) = iso639_2_to_1(&lower) {
            return two.to_string();
        }
    }

    lower
}

/// Map ISO 639-2/B and /T 3-letter codes to ISO 639-1 2-letter codes.
///
/// Covers all languages that have both a 3-letter and 2-letter code and are
/// commonly encountered in subtitle tracks, plus the full set of ISO 639-2/B
/// codes that differ from their /T counterparts.
fn iso639_2_to_1(code: &str) -> Option<&'static str> {
    // This table includes:
    // 1. All ISO 639-2/B codes that differ from /T (mapped to 2-letter)
    // 2. Common ISO 639-2/T codes (mapped to 2-letter)
    match code {
        // ISO 639-2/B codes that differ from /T
        "alb" | "sqi" => Some("sq"),
        "arm" | "hye" => Some("hy"),
        "baq" | "eus" => Some("eu"),
        "bur" | "mya" => Some("my"),
        "chi" | "zho" => Some("zh"),
        "cze" | "ces" => Some("cs"),
        "dut" | "nld" => Some("nl"),
        "fre" | "fra" => Some("fr"),
        "geo" | "kat" => Some("ka"),
        "ger" | "deu" => Some("de"),
        "gre" | "ell" => Some("el"),
        "ice" | "isl" => Some("is"),
        "mac" | "mkd" => Some("mk"),
        "mao" | "mri" => Some("mi"),
        "may" | "msa" => Some("ms"),
        "per" | "fas" => Some("fa"),
        "rum" | "ron" => Some("ro"),
        "slo" | "slk" => Some("sk"),
        "tib" | "bod" => Some("bo"),
        "wel" | "cym" => Some("cy"),

        // Common ISO 639-2/T codes (same as /B)
        "aar" => Some("aa"),
        "abk" => Some("ab"),
        "afr" => Some("af"),
        "aka" => Some("ak"),
        "amh" => Some("am"),
        "ara" => Some("ar"),
        "arg" => Some("an"),
        "asm" => Some("as"),
        "ava" => Some("av"),
        "ave" => Some("ae"),
        "aym" => Some("ay"),
        "aze" => Some("az"),
        "bak" => Some("ba"),
        "bam" => Some("bm"),
        "bel" => Some("be"),
        "ben" => Some("bn"),
        "bis" => Some("bi"),
        "bos" => Some("bs"),
        "bre" => Some("br"),
        "bul" => Some("bg"),
        "cat" => Some("ca"),
        "cha" => Some("ch"),
        "che" => Some("ce"),
        "chu" => Some("cu"),
        "chv" => Some("cv"),
        "cor" => Some("kw"),
        "cos" => Some("co"),
        "cre" => Some("cr"),
        "dan" => Some("da"),
        "div" => Some("dv"),
        "dzo" => Some("dz"),
        "eng" => Some("en"),
        "epo" => Some("eo"),
        "est" => Some("et"),
        "ewe" => Some("ee"),
        "fao" => Some("fo"),
        "fij" => Some("fj"),
        "fin" => Some("fi"),
        "fry" => Some("fy"),
        "ful" => Some("ff"),
        "gla" => Some("gd"),
        "gle" => Some("ga"),
        "glg" => Some("gl"),
        "glv" => Some("gv"),
        "grn" => Some("gn"),
        "guj" => Some("gu"),
        "hat" => Some("ht"),
        "hau" => Some("ha"),
        "hbs" => Some("sh"), // Serbo-Croatian
        "heb" => Some("he"),
        "her" => Some("hz"),
        "hin" => Some("hi"),
        "hmo" => Some("ho"),
        "hrv" => Some("hr"),
        "hun" => Some("hu"),
        "ibo" => Some("ig"),
        "ido" => Some("io"),
        "iii" => Some("ii"),
        "iku" => Some("iu"),
        "ile" => Some("ie"),
        "ina" => Some("ia"),
        "ind" => Some("id"),
        "ipk" => Some("ik"),
        "ita" => Some("it"),
        "jav" => Some("jv"),
        "jpn" => Some("ja"),
        "kal" => Some("kl"),
        "kan" => Some("kn"),
        "kas" => Some("ks"),
        "kau" => Some("kr"),
        "kaz" => Some("kk"),
        "khm" => Some("km"),
        "kik" => Some("ki"),
        "kin" => Some("rw"),
        "kir" => Some("ky"),
        "kom" => Some("kv"),
        "kon" => Some("kg"),
        "kor" => Some("ko"),
        "kua" => Some("kj"),
        "kur" => Some("ku"),
        "lao" => Some("lo"),
        "lat" => Some("la"),
        "lav" => Some("lv"),
        "lim" => Some("li"),
        "lin" => Some("ln"),
        "lit" => Some("lt"),
        "ltz" => Some("lb"),
        "lub" => Some("lu"),
        "lug" => Some("lg"),
        "mal" => Some("ml"),
        "mar" => Some("mr"),
        "mlg" => Some("mg"),
        "mlt" => Some("mt"),
        "mon" => Some("mn"),
        "nau" => Some("na"),
        "nav" => Some("nv"),
        "nbl" => Some("nr"),
        "nde" => Some("nd"),
        "ndo" => Some("ng"),
        "nep" => Some("ne"),
        "nno" => Some("nn"),
        "nob" => Some("nb"),
        "nor" => Some("no"),
        "nya" => Some("ny"),
        "oci" => Some("oc"),
        "oji" => Some("oj"),
        "ori" => Some("or"),
        "orm" => Some("om"),
        "oss" => Some("os"),
        "pan" => Some("pa"),
        "pli" => Some("pi"),
        "pol" => Some("pl"),
        "por" => Some("pt"),
        "pus" => Some("ps"),
        "que" => Some("qu"),
        "roh" => Some("rm"),
        "run" => Some("rn"),
        "rus" => Some("ru"),
        "sag" => Some("sg"),
        "san" => Some("sa"),
        "sin" => Some("si"),
        "sme" => Some("se"),
        "smo" => Some("sm"),
        "sna" => Some("sn"),
        "snd" => Some("sd"),
        "som" => Some("so"),
        "sot" => Some("st"),
        "spa" => Some("es"),
        "srd" => Some("sc"),
        "srp" => Some("sr"),
        "ssw" => Some("ss"),
        "sun" => Some("su"),
        "swa" => Some("sw"),
        "swe" => Some("sv"),
        "tah" => Some("ty"),
        "tam" => Some("ta"),
        "tat" => Some("tt"),
        "tel" => Some("te"),
        "tgk" => Some("tg"),
        "tgl" => Some("tl"),
        "tha" => Some("th"),
        "tir" => Some("ti"),
        "ton" => Some("to"),
        "tsn" => Some("tn"),
        "tso" => Some("ts"),
        "tuk" => Some("tk"),
        "tur" => Some("tr"),
        "twi" => Some("tw"),
        "uig" => Some("ug"),
        "ukr" => Some("uk"),
        "urd" => Some("ur"),
        "uzb" => Some("uz"),
        "ven" => Some("ve"),
        "vie" => Some("vi"),
        "vol" => Some("vo"),
        "wln" => Some("wa"),
        "wol" => Some("wo"),
        "xho" => Some("xh"),
        "yid" => Some("yi"),
        "yor" => Some("yo"),
        "zha" => Some("za"),
        "zul" => Some("zu"),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iso639_2t_to_1() {
        assert_eq!(normalize_language("eng"), "en");
        assert_eq!(normalize_language("fra"), "fr");
        assert_eq!(normalize_language("zho"), "zh");
        assert_eq!(normalize_language("spa"), "es");
        assert_eq!(normalize_language("jpn"), "ja");
        assert_eq!(normalize_language("kor"), "ko");
        assert_eq!(normalize_language("rus"), "ru");
    }

    #[test]
    fn test_iso639_2b_to_1() {
        // Bibliographic codes that differ from terminology
        assert_eq!(normalize_language("chi"), "zh");
        assert_eq!(normalize_language("fre"), "fr");
        assert_eq!(normalize_language("ger"), "de");
        assert_eq!(normalize_language("dut"), "nl");
        assert_eq!(normalize_language("rum"), "ro");
        assert_eq!(normalize_language("cze"), "cs");
        assert_eq!(normalize_language("gre"), "el");
        assert_eq!(normalize_language("ice"), "is");
        assert_eq!(normalize_language("mac"), "mk");
        assert_eq!(normalize_language("per"), "fa");
        assert_eq!(normalize_language("slo"), "sk");
        assert_eq!(normalize_language("tib"), "bo");
        assert_eq!(normalize_language("wel"), "cy");
        assert_eq!(normalize_language("baq"), "eu");
        assert_eq!(normalize_language("arm"), "hy");
        assert_eq!(normalize_language("bur"), "my");
        assert_eq!(normalize_language("geo"), "ka");
        assert_eq!(normalize_language("mao"), "mi");
        assert_eq!(normalize_language("may"), "ms");
        assert_eq!(normalize_language("alb"), "sq");
    }

    #[test]
    fn test_two_letter_passthrough() {
        assert_eq!(normalize_language("en"), "en");
        assert_eq!(normalize_language("zh"), "zh");
        assert_eq!(normalize_language("fr"), "fr");
    }

    #[test]
    fn test_bcp47_with_subtags() {
        assert_eq!(normalize_language("zh-Hans"), "zh-Hans");
        assert_eq!(normalize_language("zh-Hant"), "zh-Hant");
        assert_eq!(normalize_language("pt-BR"), "pt-BR");
        assert_eq!(normalize_language("zho-Hans"), "zh-Hans");
    }

    #[test]
    fn test_unknown_passthrough() {
        // Unknown 3-letter codes without a 2-letter equivalent pass through
        assert_eq!(normalize_language("qaa"), "qaa");
        assert_eq!(normalize_language("mis"), "mis");
    }

    #[test]
    fn test_case_normalization() {
        assert_eq!(normalize_language("ENG"), "en");
        assert_eq!(normalize_language("Fre"), "fr");
        assert_eq!(normalize_language("ZHO"), "zh");
    }
}
