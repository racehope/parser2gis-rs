//! Список городов 2GIS (из оригинального parser-2gis).

use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct City {
    pub name: String,
    pub code: String,
    pub domain: String,
    pub country_code: String,
}

impl City {
    pub fn search_url(&self, query: &str) -> String {
        format!(
            "https://2gis.{}/{}/search/{}",
            self.domain,
            self.code,
            percent_encode(query.trim())
        )
    }
}

pub fn country_name(cc: &str) -> &'static str {
    match cc {
        "kz" => "Казахстан",
        "ru" => "Россия",
        "by" => "Беларусь",
        "kg" => "Кыргызстан",
        "uz" => "Узбекистан",
        "az" => "Азербайджан",
        "ae" => "ОАЭ",
        "sa" => "Саудовская Аравия",
        "cz" => "Чехия",
        "it" => "Италия",
        "cy" => "Кипр",
        "cl" => "Чили",
        "eg" => "Египет",
        "qa" => "Катар",
        "om" => "Оман",
        "bh" => "Бахрейн",
        "kw" => "Кувейт",
        "iq" => "Ирак",
        _ => "",
    }
}

/// Все города: сначала Казахстан, потом Россия, потом остальные; внутри — по алфавиту.
pub fn all() -> &'static [City] {
    static CITIES: OnceLock<Vec<City>> = OnceLock::new();
    CITIES.get_or_init(|| {
        let mut v: Vec<City> =
            serde_json::from_str(include_str!("../assets/cities.json")).unwrap_or_default();
        let rank = |cc: &str| match cc {
            "kz" => 0,
            "ru" => 1,
            "by" | "kg" | "uz" => 2,
            _ => 3,
        };
        v.sort_by(|a, b| {
            rank(&a.country_code)
                .cmp(&rank(&b.country_code))
                .then_with(|| a.country_code.cmp(&b.country_code))
                .then_with(|| a.name.cmp(&b.name))
        });
        v
    })
}

pub fn find(code: &str, domain: &str) -> Option<&'static City> {
    all().iter().find(|c| c.code == code && c.domain == domain)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn cities_load_and_url() {
        let c = super::find("kostanay", "kz").expect("Костанай есть в списке");
        assert_eq!(c.name, "Костанай");
        assert_eq!(c.search_url("кофе"), "https://2gis.kz/kostanay/search/%D0%BA%D0%BE%D1%84%D0%B5");
        assert_eq!(super::all()[0].country_code, "kz");
    }
}
