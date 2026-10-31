//! Запись результатов: CSV, XLSX, JSON.
//!
//! CSV/XLSX: строки параллельно пишутся в `<output>.partial.csv` (чтобы данные
//! не терялись при падении), а в конце формируется итоговый файл с удалением
//! пустых колонок и дубликатов — как в оригинале.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use regex::Regex;
use serde_json::Value;

use crate::model::{CatalogItem, Contact};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Csv,
    Xlsx,
    Json,
}

impl Format {
    pub fn from_path(p: &Path) -> Option<Self> {
        match p.extension()?.to_str()?.to_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "xlsx" => Some(Self::Xlsx),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Encoding {
    #[value(name = "utf-8-sig")]
    Utf8Sig,
    #[value(name = "utf-8")]
    Utf8,
    #[value(name = "cp1251")]
    Cp1251,
}

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub encoding: Encoding,
    pub add_rubrics: bool,
    pub add_comments: bool,
    pub columns_per_entity: usize,
    pub remove_empty_columns: bool,
    pub remove_duplicates: bool,
    pub join_char: String,
}

pub trait Sink {
    fn write(&mut self, raw_item: &Value, item: &CatalogItem) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<usize>;
}

pub fn open(path: &Path, format: Format, opts: WriterOptions) -> Result<Box<dyn Sink>> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("не удалось создать {dir:?}"))?;
    }
    Ok(match format {
        Format::Json => Box::new(JsonSink::new(path)?),
        Format::Csv | Format::Xlsx => Box::new(TableSink::new(path, format, opts)?),
    })
}

// ---------------------------------------------------------------- JSON

struct JsonSink {
    file: BufWriter<File>,
    count: usize,
}

impl JsonSink {
    fn new(path: &Path) -> Result<Self> {
        let mut file = BufWriter::new(File::create(path).with_context(|| format!("не удалось создать {path:?}"))?);
        file.write_all(b"[")?;
        Ok(Self { file, count: 0 })
    }
}

impl Sink for JsonSink {
    fn write(&mut self, raw: &Value, _item: &CatalogItem) -> Result<()> {
        if self.count > 0 {
            self.file.write_all(b",")?;
        }
        self.file.write_all(b"\n")?;
        serde_json::to_writer(&mut self.file, raw)?;
        self.file.flush()?;
        self.count += 1;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<usize> {
        if self.count > 0 {
            self.file.write_all(b"\n")?;
        }
        self.file.write_all(b"]")?;
        self.file.flush()?;
        Ok(self.count)
    }
}

// ---------------------------------------------------------------- CSV / XLSX

const COMPLEX: &[(&str, &str)] = &[
    ("phone", "Телефон"),
    ("email", "E-mail"),
    ("website", "Веб-сайт"),
    ("instagram", "Instagram"),
    ("twitter", "Twitter"),
    ("facebook", "Facebook"),
    ("vkontakte", "ВКонтакте"),
    ("whatsapp", "WhatsApp"),
    ("viber", "Viber"),
    ("telegram", "Telegram"),
    ("youtube", "YouTube"),
    ("skype", "Skype"),
];

const NUMERIC: &[&str] = &["general_rating", "general_review_count", "branch_count", "point_lat", "point_lon"];

struct TableSink {
    path: PathBuf,
    partial_path: PathBuf,
    partial: csv::Writer<File>,
    format: Format,
    opts: WriterOptions,
    columns: Vec<(String, String)>,
    rows: Vec<Vec<String>>,
}

impl TableSink {
    fn new(path: &Path, format: Format, opts: WriterOptions) -> Result<Self> {
        let columns = Self::build_columns(&opts);
        let partial_path = path.with_extension("partial.csv");
        let mut partial = csv::Writer::from_path(&partial_path)
            .with_context(|| format!("не удалось создать {partial_path:?}"))?;
        partial.write_record(columns.iter().map(|(_, h)| h))?;
        partial.flush()?;
        Ok(Self {
            path: path.to_path_buf(),
            partial_path,
            partial,
            format,
            opts,
            columns,
            rows: Vec::new(),
        })
    }

    fn build_columns(opts: &WriterOptions) -> Vec<(String, String)> {
        let mut cols: Vec<(String, String)> = [
            ("name", "Наименование"),
            ("description", "Описание"),
            ("rubrics", "Рубрики"),
            ("address", "Адрес"),
            ("address_comment", "Комментарий к адресу"),
            ("postcode", "Почтовый индекс"),
            ("living_area", "Микрорайон"),
            ("district", "Район"),
            ("city", "Город"),
            ("district_area", "Округ"),
            ("region", "Регион"),
            ("country", "Страна"),
            ("schedule", "Часы работы"),
            ("timezone", "Часовой пояс"),
            ("general_rating", "Рейтинг"),
            ("general_review_count", "Количество отзывов"),
            ("name_lang", "Язык названия"),
            ("branch_count", "Филиалов в сети"),
        ]
        .into_iter()
        .filter(|(k, _)| opts.add_rubrics || *k != "rubrics")
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        for (k, v) in COMPLEX {
            for n in 1..=opts.columns_per_entity {
                cols.push((format!("{k}_{n}"), format!("{v} {n}")));
            }
        }
        for (k, v) in [("point_lat", "Широта"), ("point_lon", "Долгота"), ("url", "2GIS URL"), ("type", "Тип")] {
            cols.push((k.into(), v.into()));
        }
        cols
    }

    fn extract(&self, item: &CatalogItem) -> HashMap<String, String> {
        let mut d: HashMap<String, String> = HashMap::new();

        let name = item.display_name();
        if !name.is_empty() {
            d.insert("name".into(), name);
        }
        if let Some(ext) = item.name_ex.as_ref().and_then(|n| n.extension.clone()).filter(|e| !e.is_empty()) {
            d.insert("description".into(), ext);
        }
        d.insert("type".into(), item.kind.clone());
        d.insert("name_lang".into(), crate::lang::detect(&item.display_name()).short().into());
        if item.org.is_some() {
            d.insert("branch_count".into(), item.branch_count().to_string());
        }
        if let Some(a) = &item.address_name {
            d.insert("address".into(), a.clone());
        }
        if let Some(r) = &item.reviews {
            if let Some(x) = r.general_rating {
                d.insert("general_rating".into(), x.to_string());
            }
            if let Some(x) = r.general_review_count.or(r.general_review_count_with_stars) {
                d.insert("general_review_count".into(), x.to_string());
            }
        }
        if let Some(p) = &item.point {
            d.insert("point_lat".into(), p.lat.to_string());
            d.insert("point_lon".into(), p.lon.to_string());
        }
        if let Some(c) = item.address_comment.clone().filter(|c| !c.is_empty()) {
            d.insert("address_comment".into(), c);
        }
        if let Some(pc) = item.address.as_ref().and_then(|a| a.postcode.clone()) {
            d.insert("postcode".into(), pc);
        }
        if let Some(tz) = item.timezone() {
            d.insert("timezone".into(), tz);
        }
        for div in &item.adm_div {
            if ["country", "region", "district_area", "city", "district", "living_area"].contains(&div.kind.as_str()) {
                if let Some(n) = &div.name {
                    d.insert(div.kind.clone(), n.clone());
                }
            }
        }
        d.insert("url".into(), item.url());

        // Контакты. В оригинале нумерация сбрасывалась в каждой группе контактов,
        // и вторая группа перетирала первую — здесь считаем сквозь все группы.
        let mut counters: HashMap<&str, usize> = HashMap::new();
        let phone_re = Regex::new(r"[^0-9+]").unwrap();
        for c in item.contacts() {
            let Some((key, _)) = COMPLEX.iter().find(|(k, _)| *k == c.kind) else { continue };
            let Some(mut value) = contact_value(c, &phone_re) else { continue };
            if self.opts.add_comments {
                if let Some(cm) = c.comment.as_deref().filter(|x| !x.is_empty()) {
                    value.push_str(&format!(" ({cm})"));
                }
            }
            let n = counters.entry(key).or_insert(0);
            *n += 1;
            if *n <= self.opts.columns_per_entity {
                d.insert(format!("{key}_{n}"), value);
            }
        }

        if let Some(s) = &item.schedule {
            let s = s.to_str(&self.opts.join_char, self.opts.add_comments);
            if !s.is_empty() {
                d.insert("schedule".into(), s);
            }
        }
        if self.opts.add_rubrics && !item.rubrics.is_empty() {
            let r: Vec<_> = item.rubrics.iter().map(|r| r.name.as_str()).collect();
            d.insert("rubrics".into(), r.join(&self.opts.join_char));
        }
        d
    }

    fn finalize(&self) -> (Vec<(String, String)>, Vec<Vec<String>>) {
        let mut keep: Vec<usize> = (0..self.columns.len()).collect();

        if self.opts.remove_empty_columns {
            let complex_re = Regex::new(&format!(
                r"^({})_\d+$",
                COMPLEX.iter().map(|(k, _)| *k).collect::<Vec<_>>().join("|")
            ))
            .unwrap();
            keep.retain(|&i| {
                let key = &self.columns[i].0;
                !complex_re.is_match(key) || self.rows.iter().any(|r| !r[i].is_empty())
            });
        }

        let mut columns: Vec<(String, String)> = keep.iter().map(|&i| self.columns[i].clone()).collect();

        // Одиночная колонка "Телефон 1" → "Телефон"
        let keys: HashSet<String> = columns.iter().map(|(k, _)| k.clone()).collect();
        for (k, v) in COMPLEX {
            if keys.contains(&format!("{k}_1")) && !keys.contains(&format!("{k}_2")) {
                if let Some(col) = columns.iter_mut().find(|(key, _)| *key == format!("{k}_1")) {
                    col.1 = v.to_string();
                }
            }
        }

        let mut seen = HashSet::new();
        let rows: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|r| keep.iter().map(|&i| r[i].clone()).collect::<Vec<_>>())
            .filter(|r| !self.opts.remove_duplicates || seen.insert(r.clone()))
            .collect();

        (columns, rows)
    }

    fn write_csv(&self, columns: &[(String, String)], rows: &[Vec<String>]) -> Result<()> {
        let mut w = csv::Writer::from_writer(Vec::new());
        w.write_record(columns.iter().map(|(_, h)| h))?;
        for r in rows {
            w.write_record(r)?;
        }
        let data = String::from_utf8(w.into_inner()?)?;
        let bytes: Vec<u8> = match self.opts.encoding {
            Encoding::Utf8 => data.into_bytes(),
            Encoding::Utf8Sig => {
                let mut b = vec![0xEF, 0xBB, 0xBF];
                b.extend(data.into_bytes());
                b
            }
            Encoding::Cp1251 => encoding_rs::WINDOWS_1251.encode(&data).0.into_owned(),
        };
        std::fs::write(&self.path, bytes).with_context(|| format!("не удалось записать {:?}", self.path))
    }

    fn write_xlsx(&self, columns: &[(String, String)], rows: &[Vec<String>]) -> Result<()> {
        use rust_xlsxwriter::{Format as XFormat, Workbook};
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        let bold = XFormat::new().set_bold();
        for (c, (_, h)) in columns.iter().enumerate() {
            ws.write_string_with_format(0, c as u16, h, &bold)?;
        }
        for (r, row) in rows.iter().enumerate() {
            let r = (r + 1) as u32;
            for (c, val) in row.iter().enumerate() {
                if val.is_empty() {
                    continue;
                }
                let numeric = NUMERIC.contains(&columns[c].0.as_str());
                match val.parse::<f64>() {
                    Ok(n) if numeric => ws.write_number(r, c as u16, n)?,
                    _ => ws.write_string(r, c as u16, val)?,
                };
            }
        }
        ws.set_freeze_panes(1, 0)?;
        if !rows.is_empty() {
            ws.autofilter(0, 0, rows.len() as u32, columns.len().saturating_sub(1) as u16)?;
        }
        ws.set_autofit_max_width(400).autofit();
        wb.save(&self.path).with_context(|| format!("не удалось записать {:?}", self.path))?;
        Ok(())
    }
}

pub(crate) fn contact_value(c: &Contact, phone_re: &Regex) -> Option<String> {
    let nonempty = |s: &Option<String>| s.clone().filter(|v| !v.is_empty());
    match c.kind.as_str() {
        "email" | "skype" => nonempty(&c.value),
        "phone" => {
            let raw = nonempty(&c.text).or_else(|| nonempty(&c.value))?;
            let digits = phone_re.replace_all(&raw, "").to_string();
            Some(match digits.strip_prefix("+7") {
                Some(rest) => format!("8{rest}"),
                None => digits,
            })
        }
        "whatsapp" => nonempty(&c.url).map(|u| u.split('?').next().unwrap_or_default().to_string()),
        // url у сайта — редирект через link.2gis.*, value — реальный адрес
        "website" => nonempty(&c.value).or_else(|| nonempty(&c.url)),
        _ => nonempty(&c.url).or_else(|| nonempty(&c.value)),
    }
}

impl Sink for TableSink {
    fn write(&mut self, _raw: &Value, item: &CatalogItem) -> Result<()> {
        let data = self.extract(item);
        let row: Vec<String> = self
            .columns
            .iter()
            .map(|(k, _)| data.get(k).cloned().unwrap_or_default())
            .collect();
        self.partial.write_record(&row)?;
        self.partial.flush()?;
        self.rows.push(row);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<usize> {
        let (columns, rows) = self.finalize();
        match self.format {
            Format::Csv => self.write_csv(&columns, &rows)?,
            Format::Xlsx => self.write_xlsx(&columns, &rows)?,
            Format::Json => bail!("unreachable"),
        }
        let n = rows.len();
        drop(self.partial);
        let _ = std::fs::remove_file(&self.partial_path);
        Ok(n)
    }
}
