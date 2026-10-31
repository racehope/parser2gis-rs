//! Примерное определение языка названия организации: казахский / русский / латиница.
//!
//! Это эвристика по тексту названия, а не по языку владельца: казахские буквы
//! (ә ғ қ ң ө ұ ү һ і), типичные казахские слова и казахская латиница.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameLang {
    Kazakh,
    Russian,
    /// Латиница / цифры / не удалось определить
    Other,
}

impl NameLang {
    pub fn short(self) -> &'static str {
        match self {
            Self::Kazakh => "каз",
            Self::Russian => "рус",
            Self::Other => "лат",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ValueEnum)]
pub enum LangFilter {
    /// Любой язык
    #[default]
    Any,
    /// Отсеять казахские названия (латиница остаётся)
    NotKazakh,
    /// Только русские названия
    Russian,
    /// Только казахские названия
    Kazakh,
}

impl LangFilter {
    pub fn accepts(self, lang: NameLang) -> bool {
        match self {
            Self::Any => true,
            Self::NotKazakh => lang != NameLang::Kazakh,
            Self::Russian => lang == NameLang::Russian,
            Self::Kazakh => lang == NameLang::Kazakh,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Any => "любой язык",
            Self::NotKazakh => "без казахских названий",
            Self::Russian => "только русские названия",
            Self::Kazakh => "только казахские названия",
        }
    }
}

const KAZAKH_LETTERS: &[char] = &['ә', 'ғ', 'қ', 'ң', 'ө', 'ұ', 'ү', 'һ', 'і'];
const KAZAKH_LATIN_LETTERS: &[char] = &['ä', 'ğ', 'ı', 'ñ', 'ö', 'ū', 'ü', 'ş'];

/// Казахские слова, которые часто пишут без спецбукв (основы, по вхождению).
const KAZAKH_WORDS: &[&str] = &[
    "шаштараз", "сулулык", "сұлулық", "орталыгы", "орталығы", "дукени", "дүкені", "дукен",
    "асхана", "дамхана", "дәмхана", "кызмет", "қызмет", "сауда", "тұрмыс", "турмыс",
    "балалар", "білім", "жондеу", "жөндеу", "салоны", "уйі", "үйі", "ханасы", "бөлімі",
    "кафесі", "мейрамхана", "қонақ", "конак уй", "дәріхана", "дарихана", "ойын", "әлем",
    "жұлдыз", "жулдыз", "ару", "сұлу", "сулу", "нұр", "бақыт", "бакыт", "алтын", "ақ ",
    "көк", "достар", "жігіт", "жигит", "ер-азамат", "азамат",
];

/// Казахская латиница (новый алфавит и транслит).
const KAZAKH_LATIN_WORDS: &[&str] = &[
    "shashtaraz", "sulu", "ortalyq", "ortalygy", "dukeni", "qazaq", "qyzmet", "jondeu",
    "balalar", "bilim", "altyn", "nur ", "bakyt", "baqyt", "dostar", "jigit", "aru ",
];

pub fn detect(name: &str) -> NameLang {
    let lower = name.to_lowercase();
    let padded = format!(" {lower} ");

    if lower.chars().any(|c| KAZAKH_LETTERS.contains(&c)) {
        return NameLang::Kazakh;
    }
    if KAZAKH_WORDS.iter().any(|w| padded.contains(w)) {
        return NameLang::Kazakh;
    }

    let cyr = lower.chars().filter(|c| ('а'..='я').contains(c) || *c == 'ё').count();
    let lat = lower.chars().filter(|c| c.is_ascii_alphabetic() || KAZAKH_LATIN_LETTERS.contains(c)).count();

    if cyr > 0 && cyr >= lat {
        return NameLang::Russian;
    }
    if lat > 0 {
        if lower.chars().any(|c| KAZAKH_LATIN_LETTERS.contains(&c))
            || KAZAKH_LATIN_WORDS.iter().any(|w| padded.contains(w))
            || has_bare_q(&lower)
        {
            return NameLang::Kazakh;
        }
        // Смешанное «BarberShop Ахмед» — решает кириллица, если она есть
        if cyr > 0 {
            return NameLang::Russian;
        }
    }
    NameLang::Other
}

/// «q» не перед «u» почти не встречается в английском, зато типична для казахской латиницы.
fn has_bare_q(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    chars
        .iter()
        .enumerate()
        .any(|(i, &c)| c == 'q' && chars.get(i + 1) != Some(&'u'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kazakh() {
        for n in ["Шаштараз Айгерім", "Сұлулық салоны", "Шаштараз", "Ару сұлулық орталығы", "Qazaq Barber", "Shashtaraz Nur", "Altyn Shash", "Ақ Жол"] {
            assert_eq!(detect(n), NameLang::Kazakh, "{n}");
        }
    }

    #[test]
    fn russian() {
        for n in ["Барбершоп Борода", "Парикмахерская Локон", "Салон красоты Шарм", "Мужская парикмахерская", "BarberShop Сергей", "Цирюльня"] {
            assert_eq!(detect(n), NameLang::Russian, "{n}");
        }
    }

    #[test]
    fn latin_is_other() {
        for n in ["Barber King", "TOPGUN", "Chop-Chop", "OldBoy Barbershop", "777"] {
            assert_eq!(detect(n), NameLang::Other, "{n}");
        }
    }

    #[test]
    fn filters() {
        assert!(LangFilter::NotKazakh.accepts(NameLang::Other));
        assert!(!LangFilter::NotKazakh.accepts(NameLang::Kazakh));
        assert!(!LangFilter::Russian.accepts(NameLang::Other));
    }
}
