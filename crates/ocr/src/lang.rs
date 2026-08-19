//! 语言码映射：tesseract 语言码（CLI/UI 的统一入参）→ BCP-47
//! （Windows `Language::CreateLanguage` / Vision `recognitionLanguages` 用）。
//! 纯查表函数，任何平台可测。

/// 映射失败返回 None（引擎侧各自决定回退策略）。
pub(crate) fn tess_to_bcp47(code: &str) -> Option<&'static str> {
    Some(match code {
        "chi_sim" | "chs" => "zh-Hans",
        "chi_tra" | "cht" => "zh-Hant",
        "eng" => "en-US",
        "jpn" => "ja-JP",
        "kor" => "ko-KR",
        "rus" => "ru-RU",
        "fra" | "fre" => "fr-FR",
        "deu" | "ger" => "de-DE",
        "spa" => "es-ES",
        "por" => "pt-BR",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_codes() {
        assert_eq!(tess_to_bcp47("chi_sim"), Some("zh-Hans"));
        assert_eq!(tess_to_bcp47("eng"), Some("en-US"));
        assert_eq!(tess_to_bcp47("xxx"), None);
        assert_eq!(tess_to_bcp47(""), None);
    }
}
