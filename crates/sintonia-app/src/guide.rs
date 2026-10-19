//! La guida dei programmi dal lato dell'interfaccia.
//!
//! Il servizio manda i programmi di una finestra di tempo ([`Schedule`]): una attorno ad adesso
//! per «in onda / dopo» (elenco Live, player, Home) e una per il giorno mostrato nella Guida.
//! [`GuideModel`] è il modello della griglia: segue l'elenco dei canali (stessa categoria e
//! ricerca di Live) e costruisce i programmi solo delle righe che Slint disegna.
//!
//! Gli orari si mostrano nell'ora locale; la griglia misura i minuti sull'orologio del giorno
//! (nei giorni del cambio d'ora un programma resta allineato all'ora scritta).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use sintonia_core::db::{ChannelRow, GuideProgramme};
use slint::{Model, ModelNotify, ModelRc, ModelTracker, VecModel};

use crate::catalog::{format_number, CatalogModel};
use crate::{ChannelItem, DayItem, GuideBlock, GuideRow};

/// Minuti in un giorno della griglia.
const DAY_MINUTES: i64 = 24 * 60;

/// Programmi per canale (chiave stabile), in ordine di inizio.
#[derive(Debug, Default)]
pub struct Schedule {
    by_channel: HashMap<String, Vec<GuideProgramme>>,
}

impl Schedule {
    /// Dai programmi del servizio (già in ordine di canale e di inizio).
    pub fn new(programmes: &[GuideProgramme]) -> Self {
        let mut by_channel: HashMap<String, Vec<GuideProgramme>> = HashMap::new();
        for p in programmes {
            by_channel
                .entry(p.channel_key.clone())
                .or_default()
                .push(p.clone());
        }
        Self { by_channel }
    }

    /// Programmi di un canale.
    pub fn programmes(&self, key: &str) -> &[GuideProgramme] {
        self.by_channel.get(key).map_or(&[], Vec::as_slice)
    }

    /// Programma in onda a `now` e il successivo.
    pub fn now_next(
        &self,
        key: &str,
        now: i64,
    ) -> (Option<&GuideProgramme>, Option<&GuideProgramme>) {
        let list = self.programmes(key);
        // il primo programma che finisce dopo adesso
        let index = list.partition_point(|p| p.stop <= now);
        match list.get(index) {
            Some(p) if p.start <= now => (Some(p), list.get(index + 1)),
            next => (None, next),
        }
    }
}

/// Ora locale «20:30».
pub fn clock(secs: i64) -> String {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// «20:30 – 21:30».
fn span(p: &GuideProgramme) -> String {
    format!("{} – {}", clock(p.start), clock(p.stop))
}

/// Riempie i campi «in onda / dopo» di un canale.
pub fn fill_now_next(item: &mut ChannelItem, schedule: &Schedule, key: &str, now: i64) {
    let (current, next) = schedule.now_next(key, now);
    match current {
        Some(p) => {
            item.now_title = p.title.as_str().into();
            item.now_time = span(p).into();
            let length = (p.stop - p.start).max(1) as f32;
            item.now_progress = ((now - p.start) as f32 / length).clamp(0.0, 1.0);
        }
        None => {
            item.now_title = Default::default();
            item.now_time = Default::default();
            item.now_progress = -1.0;
        }
    }
    match next {
        Some(p) => {
            item.next_title = p.title.as_str().into();
            item.next_time = clock(p.start).into();
        }
        None => {
            item.next_title = Default::default();
            item.next_time = Default::default();
        }
    }
}

/// Mezzanotte locale del giorno `offset` (0 oggi, -1 ieri…) in secondi Unix, e la data.
pub fn day_start(offset: i64) -> (i64, NaiveDate) {
    let today = Local::now().date_naive();
    let date = today
        .checked_add_signed(chrono::Duration::days(offset))
        .unwrap_or(today);
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .and_then(|t| Local.from_local_datetime(&t).earliest())
        .map_or(0, |t| t.timestamp());
    (midnight, date)
}

/// Minuti sull'orologio locale da mezzanotte di `date` (negativi prima, oltre 1440 dopo).
pub fn wall_minutes(secs: i64, date: NaiveDate) -> i64 {
    let Some(t) = Local.timestamp_opt(secs, 0).single() else {
        return 0;
    };
    let days = (t.date_naive() - date).num_days();
    days * DAY_MINUTES + i64::from(t.hour()) * 60 + i64::from(t.minute())
}

/// I giorni della Guida: ieri, oggi e i sei successivi.
pub fn days() -> Vec<DayItem> {
    (-1..=6)
        .map(|offset| {
            let (_, date) = day_start(offset);
            DayItem {
                offset: i32::try_from(offset).unwrap_or(0),
                weekday: i32::try_from(date.weekday().num_days_from_monday()).unwrap_or(0),
                day: i32::try_from(date.day()).unwrap_or(1),
            }
        })
        .collect()
}

/// I blocchi di un canale in un giorno (`date`), con lo stato rispetto a `now`.
pub fn blocks(programmes: &[GuideProgramme], date: NaiveDate, now: i64) -> Vec<GuideBlock> {
    programmes
        .iter()
        .filter_map(|p| {
            let start = wall_minutes(p.start, date);
            let stop = wall_minutes(p.stop, date);
            if stop <= 0 || start >= DAY_MINUTES || stop <= start {
                return None;
            }
            Some(GuideBlock {
                title: p.title.as_str().into(),
                time: span(p).into(),
                start: i32::try_from(start).unwrap_or(0),
                len: i32::try_from(stop - start).unwrap_or(0),
                now: p.start <= now && now < p.stop,
                past: p.stop <= now,
            })
        })
        .collect()
}

/// Il modello della griglia della Guida (vedi la documentazione del modulo).
pub struct GuideModel {
    channels: Rc<CatalogModel<ChannelRow>>,
    schedule: RefCell<Schedule>,
    date: Cell<NaiveDate>,
    now: Cell<i64>,
    playing: Cell<Option<i64>>,
    notify: ModelNotify,
}

impl std::fmt::Debug for GuideModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuideModel")
            .field("date", &self.date.get())
            .finish_non_exhaustive()
    }
}

impl GuideModel {
    /// Modello sopra l'elenco dei canali.
    pub fn new(channels: Rc<CatalogModel<ChannelRow>>) -> Rc<Self> {
        Rc::new(Self {
            channels,
            schedule: RefCell::new(Schedule::default()),
            date: Cell::new(Local::now().date_naive()),
            now: Cell::new(0),
            playing: Cell::new(None),
            notify: ModelNotify::default(),
        })
    }

    /// Il giorno mostrato e i suoi programmi.
    pub fn set_day(&self, date: NaiveDate, schedule: Schedule) {
        self.date.set(date);
        *self.schedule.borrow_mut() = schedule;
        self.notify.reset();
    }

    /// Ora attuale (per i programmi passati e in onda) e canale in riproduzione.
    pub fn set_now(&self, now: i64, playing: Option<i64>) {
        self.now.set(now);
        self.playing.set(playing);
        self.notify.reset();
    }

    /// L'elenco dei canali è cambiato (categoria, ricerca, catalogo).
    pub fn refresh(&self) {
        self.notify.reset();
    }
}

impl Model for GuideModel {
    type Data = GuideRow;

    fn row_count(&self) -> usize {
        self.channels.row_count()
    }

    fn row_data(&self, row: usize) -> Option<GuideRow> {
        let (id, key) = self.channels.entry_at(row)?;
        let (num, name) = self
            .channels
            .with_entry(id, |c| (format_number(c.number), c.name.clone()))?;
        let blocks = blocks(
            self.schedule.borrow().programmes(&key),
            self.date.get(),
            self.now.get(),
        );
        Some(GuideRow {
            id: i32::try_from(id).unwrap_or(-1),
            num: num.into(),
            name: name.into(),
            playing: self.playing.get() == Some(id),
            blocks: ModelRc::new(VecModel::from(blocks)),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programme(key: &str, start: i64, stop: i64, title: &str) -> GuideProgramme {
        GuideProgramme {
            channel_key: key.into(),
            start,
            stop,
            title: title.into(),
            subtitle: None,
            category: None,
        }
    }

    #[test]
    fn now_and_next() {
        let schedule = Schedule::new(&[
            programme("a", 100, 200, "Uno"),
            programme("a", 200, 300, "Due"),
            programme("a", 400, 500, "Tre"),
            programme("b", 0, 1000, "Lungo"),
        ]);
        let titles = |now| {
            let (c, n) = schedule.now_next("a", now);
            (c.map(|p| p.title.clone()), n.map(|p| p.title.clone()))
        };
        assert_eq!(titles(150), (Some("Uno".into()), Some("Due".into())));
        assert_eq!(titles(200), (Some("Due".into()), Some("Tre".into())));
        assert_eq!(
            titles(350),
            (None, Some("Tre".into())),
            "buco tra due programmi"
        );
        assert_eq!(titles(600), (None, None));
        assert_eq!(schedule.now_next("x", 0), (None, None));

        let mut item = ChannelItem::default();
        fill_now_next(&mut item, &schedule, "a", 150);
        assert_eq!(item.now_title.as_str(), "Uno");
        assert!((item.now_progress - 0.5).abs() < 1e-6);
        fill_now_next(&mut item, &schedule, "a", 350);
        assert_eq!(item.now_title.as_str(), "");
        assert!(item.now_progress < 0.0);
        assert_eq!(item.next_title.as_str(), "Tre");
    }

    #[test]
    fn blocks_are_clipped_to_the_day() {
        let (midnight, date) = day_start(0);
        let list = [
            programme("a", midnight - 1800, midnight + 1800, "A cavallo"),
            programme("a", midnight + 3600, midnight + 7200, "Dopo"),
            programme("a", midnight - 7200, midnight - 3600, "Ieri"),
            programme("a", midnight + 25 * 3600, midnight + 26 * 3600, "Domani"),
        ];
        let blocks = blocks(&list, date, midnight + 3700);
        let found: Vec<(&str, i32, i32, bool, bool)> = blocks
            .iter()
            .map(|b| (b.title.as_str(), b.start, b.len, b.now, b.past))
            .collect();
        // il primo inizia ieri sera: comincia a -30 minuti, dura un'ora
        assert_eq!(
            found,
            [
                ("A cavallo", -30, 60, false, true),
                ("Dopo", 60, 60, true, false)
            ]
        );
    }

    #[test]
    fn days_start_yesterday() {
        let days = days();
        assert_eq!(days.len(), 8);
        assert_eq!(days[0].offset, -1);
        assert_eq!(days[1].offset, 0);
        let (start, date) = day_start(0);
        assert_eq!(wall_minutes(start, date), 0);
        assert_eq!(wall_minutes(start + 90 * 60, date), 90);
    }
}
