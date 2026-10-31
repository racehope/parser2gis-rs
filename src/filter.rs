//! Фильтры записей: по наличию/отсутствию контактов и по отзывам/рейтингу.

use std::collections::HashSet;

use clap::ValueEnum;

use crate::lang::{self, LangFilter};
use crate::model::{CatalogItem, Contact};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum ContactKind {
    Website,
    Instagram,
    Whatsapp,
    Telegram,
    Vkontakte,
    Facebook,
    Viber,
    Youtube,
    Phone,
    Email,
}

impl ContactKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Website => "сайт",
            Self::Instagram => "Instagram",
            Self::Whatsapp => "WhatsApp",
            Self::Telegram => "Telegram",
            Self::Vkontakte => "ВКонтакте",
            Self::Facebook => "Facebook",
            Self::Viber => "Viber",
            Self::Youtube => "YouTube",
            Self::Phone => "телефон",
            Self::Email => "e-mail",
        }
    }

    /// Родительный падеж — для "нет …"
    pub fn label_gen(self) -> &'static str {
        match self {
            Self::Website => "сайта",
            Self::Phone => "телефона",
            other => other.label(),
        }
    }

    fn from_2gis_type(t: &str) -> Option<Self> {
        Some(match t {
            "website" => Self::Website,
            "instagram" => Self::Instagram,
            "whatsapp" => Self::Whatsapp,
            "telegram" => Self::Telegram,
            "vkontakte" => Self::Vkontakte,
            "facebook" => Self::Facebook,
            "viber" => Self::Viber,
            "youtube" => Self::Youtube,
            "phone" => Self::Phone,
            "email" => Self::Email,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum WithoutMode {
    /// Нет НИ ОДНОГО из перечисленных контактов
    #[default]
    All,
    /// Нет ХОТЯ БЫ ОДНОГО из перечисленных контактов
    Any,
}

/// Классификация "сайта": в 2GIS в поле website часто лежит ссылка на соцсеть
/// или link-in-bio. Возвращает `Some(kind)` для соцсети, `None` если это
/// мусорная ссылка (taplink и т.п.), и `Some(Website)` для настоящего сайта.
pub fn classify_website(c: &Contact) -> Option<ContactKind> {
    // value — техническое значение (обычно "http://domain"), text — то, что видит
    // пользователь; url — редирект через link.2gis.*, поэтому его не учитываем.
    let raw = [c.value.as_deref(), c.text.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    let has = |needles: &[&str]| needles.iter().any(|n| raw.contains(n));

    if has(&["instagram.com", "instagr.am", "ig.me"]) {
        Some(ContactKind::Instagram)
    } else if has(&["wa.me", "whatsapp.com", "wa.link", "chat.whatsapp"]) {
        Some(ContactKind::Whatsapp)
    } else if has(&["t.me/", "telegram.me", "telegram.org"]) {
        Some(ContactKind::Telegram)
    } else if has(&["vk.com", "vk.ru", "vk.me"]) {
        Some(ContactKind::Vkontakte)
    } else if has(&["facebook.com", "fb.com", "fb.me"]) {
        Some(ContactKind::Facebook)
    } else if has(&["youtube.com", "youtu.be"]) {
        Some(ContactKind::Youtube)
    } else if has(&[
        "taplink.",
        "linktr.ee",
        "hipolink.",
        "mssg.me",
        "msg.link",
        "tap.link",
        "lnk.bio",
        "linkin.bio",
    ]) {
        None
    } else {
        Some(ContactKind::Website)
    }
}

/// Какие типы контактов есть у организации.
pub fn present_kinds(item: &CatalogItem, strict_website: bool) -> HashSet<ContactKind> {
    let mut out = HashSet::new();
    for c in item.contacts() {
        let Some(kind) = ContactKind::from_2gis_type(&c.kind) else {
            continue;
        };
        if kind == ContactKind::Website && !strict_website {
            if let Some(k) = classify_website(c) {
                out.insert(k);
            }
        } else {
            out.insert(kind);
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub without: Vec<ContactKind>,
    pub without_mode: WithoutMode,
    pub with: Vec<ContactKind>,
    pub min_reviews: Option<i64>,
    pub max_reviews: Option<i64>,
    pub min_rating: Option<f64>,
    pub max_rating: Option<f64>,
    pub strict_website: bool,
    pub include_non_firms: bool,
    pub lang: LangFilter,
    /// Записывать только один филиал от каждой сети
    pub one_per_org: bool,
}

impl Filters {
    pub fn is_active(&self) -> bool {
        !self.without.is_empty()
            || !self.with.is_empty()
            || self.min_reviews.is_some()
            || self.max_reviews.is_some()
            || self.min_rating.is_some()
            || self.max_rating.is_some()
            || self.lang != LangFilter::Any
    }

    /// `Ok(())` — запись проходит, `Err(причина)` — отсеяна.
    pub fn check(&self, item: &CatalogItem) -> Result<(), String> {
        if !self.is_active() {
            return Ok(());
        }

        // Остановки, улицы, парковки и т.п. "без сайта" по определению —
        // для лидогенерации это мусор, поэтому при активных фильтрах
        // пропускаем только организации (type == "branch").
        if !self.include_non_firms && item.kind != "branch" {
            return Err(format!("не организация ({})", item.kind));
        }

        let present = present_kinds(item, self.strict_website);

        if !self.without.is_empty() {
            let has: Vec<_> = self.without.iter().filter(|k| present.contains(k)).collect();
            let pass = match self.without_mode {
                WithoutMode::All => has.is_empty(),
                WithoutMode::Any => has.len() < self.without.len(),
            };
            if !pass {
                let names: Vec<_> = has.iter().map(|k| k.label()).collect();
                return Err(format!("есть {}", names.join(", ")));
            }
        }

        if let Some(missing) = self.with.iter().find(|k| !present.contains(k)) {
            return Err(format!("нет {}", missing.label_gen()));
        }

        let reviews = item.review_count();
        if let Some(min) = self.min_reviews {
            if reviews < min {
                return Err(format!("отзывов {reviews} < {min}"));
            }
        }
        if let Some(max) = self.max_reviews {
            if reviews > max {
                return Err(format!("отзывов {reviews} > {max}"));
            }
        }

        if self.lang != LangFilter::Any {
            let l = lang::detect(&item.display_name());
            if !self.lang.accepts(l) {
                return Err(format!("название: {}", match l {
                    lang::NameLang::Kazakh => "казахское",
                    lang::NameLang::Russian => "русское",
                    lang::NameLang::Other => "латиница",
                }));
            }
        }

        let rating = item.rating();
        if let Some(min) = self.min_rating {
            match rating {
                Some(r) if r >= min => {}
                Some(r) => return Err(format!("рейтинг {r} < {min}")),
                None => return Err("нет рейтинга".into()),
            }
        }
        if let Some(max) = self.max_rating {
            if let Some(r) = rating {
                if r > max {
                    return Err(format!("рейтинг {r} > {max}"));
                }
            }
        }

        Ok(())
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.without.is_empty() {
            let names: Vec<_> = self.without.iter().map(|k| k.label_gen()).collect();
            let joiner = match self.without_mode {
                WithoutMode::All => " и ",
                WithoutMode::Any => " или ",
            };
            parts.push(format!("нет {}", names.join(joiner)));
        }
        if !self.with.is_empty() {
            let names: Vec<_> = self.with.iter().map(|k| k.label()).collect();
            parts.push(format!("есть {}", names.join(" и ")));
        }
        match (self.min_reviews, self.max_reviews) {
            (Some(a), Some(b)) => parts.push(format!("отзывов {a}..{b}")),
            (Some(a), None) => parts.push(format!("отзывов от {a}")),
            (None, Some(b)) => parts.push(format!("отзывов до {b}")),
            _ => {}
        }
        match (self.min_rating, self.max_rating) {
            (Some(a), Some(b)) => parts.push(format!("рейтинг {a}..{b}")),
            (Some(a), None) => parts.push(format!("рейтинг от {a}")),
            (None, Some(b)) => parts.push(format!("рейтинг до {b}")),
            _ => {}
        }
        if self.lang != LangFilter::Any {
            parts.push(self.lang.describe().to_string());
        }
        if self.one_per_org {
            parts.push("один филиал на сеть".into());
        }
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(contacts: serde_json::Value, reviews: Option<i64>, rating: Option<f64>) -> CatalogItem {
        serde_json::from_value(json!({
            "id": "1_x",
            "type": "branch",
            "contact_groups": [{ "contacts": contacts }],
            "reviews": { "general_review_count": reviews, "general_rating": rating }
        }))
        .unwrap()
    }

    fn f() -> Filters {
        Filters::default()
    }

    #[test]
    fn no_website() {
        let fl = Filters { without: vec![ContactKind::Website], ..f() };
        let with_site = item(json!([{"type":"website","value":"http://barber.kz","text":"barber.kz"}]), None, None);
        let no_site = item(json!([{"type":"phone","value":"+77001112233"}]), None, None);
        assert!(fl.check(&with_site).is_err());
        assert!(fl.check(&no_site).is_ok());
    }

    #[test]
    fn instagram_in_website_field_is_not_a_website() {
        let fl = Filters { without: vec![ContactKind::Website], ..f() };
        let it = item(json!([{"type":"website","value":"http://instagram.com/barber","text":"instagram.com/barber"}]), None, None);
        assert!(fl.check(&it).is_ok());
        // ...но засчитывается как Instagram
        let fl2 = Filters { without: vec![ContactKind::Instagram], ..f() };
        assert!(fl2.check(&it).is_err());
        // строгий режим: любая ссылка в поле website — сайт
        let strict = Filters { without: vec![ContactKind::Website], strict_website: true, ..f() };
        assert!(strict.check(&it).is_err());
    }

    #[test]
    fn taplink_is_not_a_website() {
        let fl = Filters { without: vec![ContactKind::Website], ..f() };
        let it = item(json!([{"type":"website","value":"https://taplink.cc/barber"}]), None, None);
        assert!(fl.check(&it).is_ok());
    }

    #[test]
    fn without_modes() {
        let ig_only = item(json!([{"type":"instagram","value":"barber"}]), None, None);
        let none = item(json!([]), None, None);
        let both = item(json!([{"type":"instagram","value":"a"},{"type":"whatsapp","value":"b"}]), None, None);

        let all = Filters { without: vec![ContactKind::Instagram, ContactKind::Whatsapp], ..f() };
        assert!(all.check(&ig_only).is_err());
        assert!(all.check(&none).is_ok());
        assert!(all.check(&both).is_err());

        let any = Filters { without_mode: WithoutMode::Any, ..all };
        assert!(any.check(&ig_only).is_ok());
        assert!(any.check(&none).is_ok());
        assert!(any.check(&both).is_err());
    }

    #[test]
    fn reviews_and_rating() {
        let fl = Filters { min_reviews: Some(10), max_reviews: Some(100), min_rating: Some(4.0), ..f() };
        assert!(fl.check(&item(json!([]), Some(50), Some(4.5))).is_ok());
        assert!(fl.check(&item(json!([]), Some(5), Some(4.5))).is_err());
        assert!(fl.check(&item(json!([]), Some(500), Some(4.5))).is_err());
        assert!(fl.check(&item(json!([]), Some(50), Some(3.9))).is_err());
        assert!(fl.check(&item(json!([]), None, None)).is_err());
    }

    #[test]
    fn require_phone_and_skip_non_firms() {
        let fl = Filters { with: vec![ContactKind::Phone], ..f() };
        assert!(fl.check(&item(json!([]), None, None)).is_err());
        assert!(fl.check(&item(json!([{"type":"phone","value":"1"}]), None, None)).is_ok());

        let stop: CatalogItem = serde_json::from_value(json!({"id":"1","type":"station"})).unwrap();
        let fl2 = Filters { without: vec![ContactKind::Website], ..f() };
        assert!(fl2.check(&stop).is_err());
        let fl3 = Filters { include_non_firms: true, ..fl2 };
        assert!(fl3.check(&stop).is_ok());
    }

    #[test]
    fn null_fields_dont_break() {
        let it: CatalogItem = serde_json::from_value(json!({
            "id": "1", "type": "branch", "contact_groups": null, "rubrics": null, "reviews": null
        }))
        .unwrap();
        assert_eq!(it.review_count(), 0);
    }
}
