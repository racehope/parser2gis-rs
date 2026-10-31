//! Модели документа каталога 2GIS (`catalog.api.2gis.*/items/byid`).
//!
//! Все поля опциональны и толерантны к `null`, чтобы изменения в API
//! не роняли парсинг целиком — неизвестные поля просто игнорируются.

use serde::{Deserialize, Deserializer};

fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CatalogItem {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    pub address: Option<Address>,
    pub address_comment: Option<String>,
    pub address_name: Option<String>,
    #[serde(deserialize_with = "null_default")]
    pub adm_div: Vec<AdmDiv>,
    #[serde(deserialize_with = "null_default")]
    pub contact_groups: Vec<ContactGroup>,
    pub name: Option<String>,
    pub name_ex: Option<NameEx>,
    pub reviews: Option<Reviews>,
    pub org: Option<Org>,
    pub point: Option<Point>,
    #[serde(deserialize_with = "null_default")]
    pub rubrics: Vec<Rubric>,
    pub schedule: Option<Schedule>,
    pub timezone_offset: Option<i64>,
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Address {
    pub postcode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AdmDiv {
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContactGroup {
    #[serde(deserialize_with = "null_default")]
    pub contacts: Vec<Contact>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Contact {
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
    pub value: Option<String>,
    pub text: Option<String>,
    pub url: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NameEx {
    pub primary: Option<String>,
    pub extension: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Reviews {
    pub general_rating: Option<f64>,
    pub general_review_count: Option<i64>,
    pub general_review_count_with_stars: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Org {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    pub name: Option<String>,
    pub branch_count: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Rubric {
    #[serde(deserialize_with = "null_default")]
    pub name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WorkingHour {
    #[serde(deserialize_with = "null_default")]
    pub from: String,
    #[serde(deserialize_with = "null_default")]
    pub to: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ScheduleDay {
    #[serde(deserialize_with = "null_default")]
    pub working_hours: Vec<WorkingHour>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[allow(non_snake_case)]
pub struct Schedule {
    pub Mon: Option<ScheduleDay>,
    pub Tue: Option<ScheduleDay>,
    pub Wed: Option<ScheduleDay>,
    pub Thu: Option<ScheduleDay>,
    pub Fri: Option<ScheduleDay>,
    pub Sat: Option<ScheduleDay>,
    pub Sun: Option<ScheduleDay>,
    pub comment: Option<String>,
}

impl Schedule {
    pub fn to_str(&self, join_char: &str, add_comment: bool) -> String {
        let days = [
            ("Пн", &self.Mon),
            ("Вт", &self.Tue),
            ("Ср", &self.Wed),
            ("Чт", &self.Thu),
            ("Пт", &self.Fri),
            ("Сб", &self.Sat),
            ("Вс", &self.Sun),
        ];
        let slots: Vec<String> = days
            .iter()
            .filter_map(|(name, day)| {
                let day = day.as_ref()?;
                let hours: Vec<String> = day
                    .working_hours
                    .iter()
                    .map(|h| format!("{}-{}", h.from, h.to))
                    .collect();
                Some(format!("{name}: {}", hours.join(", ")))
            })
            .collect();
        let mut out = slots.join(join_char);
        if add_comment {
            if let Some(c) = self.comment.as_deref().filter(|c| !c.is_empty()) {
                out.push_str(&format!(" ({c})"));
            }
        }
        out
    }
}

impl CatalogItem {
    pub fn url(&self) -> String {
        format!(
            "https://2gis.com/firm/{}",
            self.id.split('_').next().unwrap_or_default()
        )
    }

    pub fn timezone(&self) -> Option<String> {
        let off = self.timezone_offset?;
        let sign = if off < 0 { '-' } else { '+' };
        let m = off.abs();
        Some(format!("{sign}{:02}:{:02}", m / 60, m % 60))
    }

    /// Отображаемое имя (как в оригинале: name_ex.primary → name → тип объекта).
    pub fn display_name(&self) -> String {
        if let Some(p) = self.name_ex.as_ref().and_then(|n| n.primary.clone()) {
            return p;
        }
        if let Some(n) = &self.name {
            return n.clone();
        }
        match self.kind.as_str() {
            "parking" => "Парковка",
            "street" => "Улица",
            "road" => "Дорога",
            "crossroad" => "Перекрёсток",
            "station" => "Остановка",
            _ => "",
        }
        .to_string()
    }

    /// ID филиала (первая часть `id` до `_`)
    pub fn branch_id(&self) -> &str {
        self.id.split('_').next().unwrap_or_default()
    }

    /// Ключ организации (сети) для отсева филиалов
    pub fn org_key(&self) -> Option<String> {
        self.org.as_ref().map(|o| o.id.clone()).filter(|id| !id.is_empty())
    }

    pub fn branch_count(&self) -> i64 {
        self.org.as_ref().and_then(|o| o.branch_count).unwrap_or(1)
    }

    pub fn review_count(&self) -> i64 {
        self.reviews
            .as_ref()
            .and_then(|r| r.general_review_count.or(r.general_review_count_with_stars))
            .unwrap_or(0)
    }

    pub fn rating(&self) -> Option<f64> {
        self.reviews.as_ref().and_then(|r| r.general_rating)
    }

    pub fn contacts(&self) -> impl Iterator<Item = &Contact> {
        self.contact_groups.iter().flat_map(|g| g.contacts.iter())
    }
}
