//! Il modello dei canali per l'interfaccia.
//!
//! Tiene tutti i canali della sorgente in uso (arrivano dal servizio in un `Arc`, senza copie) e
//! un elenco di indici visibili, filtrato per categoria o per ricerca. Slint chiede solo le righe
//! che disegna (l'elenco è virtualizzato), quindi anche con 50 mila canali si costruiscono poche
//! decine di `ChannelItem` alla volta. I loghi si chiedono la prima volta che una riga compare.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use sintonia_core::db::ChannelRow;
use slint::{Color, Image, Model, ModelNotify, ModelTracker, SharedString};

use crate::{CategoryItem, ChannelItem};

/// Stato del logo di un indirizzo.
enum LogoState {
    Requested,
    Ready(Image),
    Missing,
}

/// Filtro attivo sull'elenco.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Filter {
    /// Indice nella lista delle categorie (0 = tutti i canali).
    Category(usize),
    /// Risultati di una ricerca (indici in `all`).
    Search(Vec<usize>),
}

/// I canali della sorgente in uso, filtrati. Implementa [`Model`] per Slint.
pub struct ChannelModel {
    all: RefCell<Arc<Vec<ChannelRow>>>,
    /// Indici di `all` per categoria: 0 = tutti, poi una voce per categoria (vedi `categories`).
    by_category: RefCell<Vec<Vec<usize>>>,
    categories: RefCell<Vec<CategoryItem>>,
    by_id: RefCell<HashMap<i64, usize>>,
    filter: RefCell<Filter>,
    visible: RefCell<Vec<usize>>,
    logos: RefCell<HashMap<String, LogoState>>,
    request_logo: Box<dyn Fn(String)>,
    notify: ModelNotify,
    /// I loghi che arrivano a raffica producono un solo aggiornamento.
    refresh_timer: slint::Timer,
    refresh_pending: Cell<bool>,
}

impl std::fmt::Debug for ChannelModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelModel")
            .field("channels", &self.all.borrow().len())
            .field("visible", &self.visible.borrow().len())
            .finish_non_exhaustive()
    }
}

/// Colori di riserva per canali senza logo (scelti dal nome, sempre uguali per lo stesso canale).
const TINTS: [u32; 10] = [
    0x3b82f6, 0x8b5cf6, 0xef4444, 0x22c55e, 0xf59e0b, 0x14b8a6, 0xec4899, 0xa855f7, 0x0ea5e9,
    0xf97316,
];

fn tint_for(name: &str) -> Color {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    let rgb = TINTS[(hasher.finish() % TINTS.len() as u64) as usize];
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Numero del canale come lo mostra l'interfaccia ("007", "1250").
pub fn format_number(number: u32) -> String {
    format!("{number:03}")
}

impl ChannelModel {
    /// Modello vuoto; `request_logo` viene chiamata per ogni logo da scaricare.
    pub fn new(request_logo: impl Fn(String) + 'static) -> Rc<Self> {
        Rc::new(Self {
            all: RefCell::new(Arc::new(Vec::new())),
            by_category: RefCell::new(vec![Vec::new()]),
            categories: RefCell::new(Vec::new()),
            by_id: RefCell::new(HashMap::new()),
            filter: RefCell::new(Filter::Category(0)),
            visible: RefCell::new(Vec::new()),
            logos: RefCell::new(HashMap::new()),
            request_logo: Box::new(request_logo),
            notify: ModelNotify::default(),
            refresh_timer: slint::Timer::default(),
            refresh_pending: Cell::new(false),
        })
    }

    /// Sostituisce tutti i canali (nuova sorgente o aggiornamento). Torna su «Tutti i canali».
    pub fn set_channels(&self, channels: Arc<Vec<ChannelRow>>) {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        let mut group_index: HashMap<&str, usize> = HashMap::new();
        let mut by_id = HashMap::with_capacity(channels.len());
        for (i, row) in channels.iter().enumerate() {
            by_id.insert(row.id, i);
            let slot = *group_index.entry(row.group.as_str()).or_insert_with(|| {
                groups.push((row.group.clone(), Vec::new()));
                groups.len() - 1
            });
            groups[slot].1.push(i);
        }
        let mut categories = Vec::with_capacity(groups.len() + 1);
        let mut by_category = Vec::with_capacity(groups.len() + 1);
        categories.push(CategoryItem {
            name: SharedString::new(),
            count: count_i32(channels.len()),
            kind: 0,
        });
        by_category.push((0..channels.len()).collect());
        // senza categoria in fondo
        groups.sort_by_key(|(name, _)| name.is_empty());
        for (name, indices) in groups {
            categories.push(CategoryItem {
                kind: if name.is_empty() { 2 } else { 1 },
                name: name.into(),
                count: count_i32(indices.len()),
            });
            by_category.push(indices);
        }
        *self.all.borrow_mut() = channels;
        *self.by_id.borrow_mut() = by_id;
        *self.categories.borrow_mut() = categories;
        *self.by_category.borrow_mut() = by_category;
        self.apply(Filter::Category(0));
    }

    /// Le categorie, con «Tutti i canali» in testa.
    pub fn categories(&self) -> Vec<CategoryItem> {
        self.categories.borrow().clone()
    }

    /// Mostra una categoria.
    pub fn show_category(&self, index: usize) {
        let index = index.min(self.by_category.borrow().len().saturating_sub(1));
        self.apply(Filter::Category(index));
    }

    /// Mostra i risultati di una ricerca (identificativi dei canali, nell'ordine della playlist).
    pub fn show_search(&self, ids: &[i64]) {
        let by_id = self.by_id.borrow();
        let indices = ids.iter().filter_map(|id| by_id.get(id).copied()).collect();
        drop(by_id);
        self.apply(Filter::Search(indices));
    }

    /// Categoria selezionata (0 durante una ricerca).
    pub fn category(&self) -> usize {
        match &*self.filter.borrow() {
            Filter::Category(i) => *i,
            Filter::Search(_) => 0,
        }
    }

    fn apply(&self, filter: Filter) {
        let visible = match &filter {
            Filter::Category(i) => self
                .by_category
                .borrow()
                .get(*i)
                .cloned()
                .unwrap_or_default(),
            Filter::Search(indices) => indices.clone(),
        };
        *self.filter.borrow_mut() = filter;
        *self.visible.borrow_mut() = visible;
        self.notify.reset();
    }

    /// Identificativo del canale alla riga `row` dell'elenco visibile.
    pub fn id_at(&self, row: usize) -> Option<i64> {
        let index = *self.visible.borrow().get(row)?;
        self.all.borrow().get(index).map(|c| c.id)
    }

    /// Riga dell'elenco visibile che contiene il canale `id`.
    pub fn row_of(&self, id: i64) -> Option<usize> {
        let index = *self.by_id.borrow().get(&id)?;
        self.visible.borrow().iter().position(|i| *i == index)
    }

    /// Il canale `id` come lo mostra l'interfaccia.
    pub fn item(&self, id: i64) -> Option<ChannelItem> {
        let index = *self.by_id.borrow().get(&id)?;
        self.item_at_index(index)
    }

    /// Cerca un canale per nome (dopo un aggiornamento gli identificativi cambiano).
    pub fn find_by_name(&self, name: &str) -> Option<i64> {
        self.all
            .borrow()
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.id)
    }

    /// Nome del canale `id`.
    pub fn name_of(&self, id: i64) -> Option<String> {
        let index = *self.by_id.borrow().get(&id)?;
        self.all.borrow().get(index).map(|c| c.name.clone())
    }

    /// Primo canale della sorgente.
    pub fn first_id(&self) -> Option<i64> {
        self.all.borrow().first().map(|c| c.id)
    }

    fn item_at_index(&self, index: usize) -> Option<ChannelItem> {
        let all = self.all.borrow();
        let row = all.get(index)?;
        let (logo, has_logo) = match row.logo.as_deref() {
            Some(url) => self.logo(url),
            None => (Image::default(), false),
        };
        Some(ChannelItem {
            id: i32::try_from(row.id).unwrap_or(-1),
            num: format_number(row.number).into(),
            name: row.name.as_str().into(),
            group: row.group.as_str().into(),
            logo,
            has_logo,
            tint: tint_for(&row.name),
            radio: row.radio,
            drm: row.drm,
        })
    }

    /// Logo pronto, oppure lo chiede (una volta sola).
    fn logo(&self, url: &str) -> (Image, bool) {
        let mut logos = self.logos.borrow_mut();
        match logos.get(url) {
            Some(LogoState::Ready(image)) => (image.clone(), true),
            Some(LogoState::Requested | LogoState::Missing) => (Image::default(), false),
            None => {
                logos.insert(url.to_owned(), LogoState::Requested);
                drop(logos);
                (self.request_logo)(url.to_owned());
                (Image::default(), false)
            }
        }
    }

    /// Un logo è arrivato (o non è disponibile).
    pub fn logo_ready(self: &Rc<Self>, url: String, image: Option<Image>) {
        let state = image.map_or(LogoState::Missing, LogoState::Ready);
        self.logos.borrow_mut().insert(url, state);
        if self.refresh_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.refresh_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(80),
            move || {
                if let Some(model) = weak.upgrade() {
                    model.refresh_pending.set(false);
                    // con l'elenco virtualizzato si ricalcolano solo le righe visibili
                    model.notify.reset();
                }
            },
        );
    }
}

fn count_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

impl Model for ChannelModel {
    type Data = ChannelItem;

    fn row_count(&self) -> usize {
        self.visible.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<ChannelItem> {
        let index = *self.visible.borrow().get(row)?;
        self.item_at_index(index)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, name: &str, group: &str, logo: Option<&str>) -> ChannelRow {
        ChannelRow {
            id,
            number: u32::try_from(id).unwrap_or(0),
            name: name.into(),
            group: group.into(),
            logo: logo.map(Into::into),
            tvg_id: None,
            radio: false,
            drm: false,
        }
    }

    fn model() -> (Rc<ChannelModel>, Rc<RefCell<Vec<String>>>) {
        let requested = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&requested);
        let model = ChannelModel::new(move |url| sink.borrow_mut().push(url));
        model.set_channels(Arc::new(vec![
            row(1, "Uno", "News", Some("http://l/1.png")),
            row(2, "Due", "", None),
            row(3, "Tre", "Sport", Some("http://l/3.png")),
            row(4, "Quattro", "News", Some("http://l/1.png")),
        ]));
        (model, requested)
    }

    #[test]
    fn categories_keep_playlist_order_with_uncategorized_last() {
        let (model, _) = model();
        let cats = model.categories();
        let names: Vec<(&str, i32, i32)> = cats
            .iter()
            .map(|c| (c.name.as_str(), c.count, c.kind))
            .collect();
        assert_eq!(
            names,
            [("", 4, 0), ("News", 2, 1), ("Sport", 1, 1), ("", 1, 2)]
        );
    }

    #[test]
    fn category_and_search_filters() {
        let (model, _) = model();
        assert_eq!(model.row_count(), 4);
        model.show_category(1);
        assert_eq!(model.row_count(), 2);
        assert_eq!(model.id_at(1), Some(4));
        assert_eq!(model.row_of(4), Some(1));
        assert_eq!(model.row_of(2), None);
        model.show_search(&[3, 99, 1]);
        assert_eq!((model.row_count(), model.id_at(0)), (2, Some(3)));
        model.show_category(42);
        assert_eq!(
            model.row_count(),
            1,
            "indice fuori misura: ultima categoria"
        );
    }

    #[test]
    fn logos_are_requested_once_per_url() {
        let (model, requested) = model();
        for row in 0..model.row_count() {
            let _ = model.row_data(row);
        }
        let _ = model.row_data(0);
        assert_eq!(*requested.borrow(), ["http://l/1.png", "http://l/3.png"]);
        model.logo_ready("http://l/1.png".into(), Some(Image::default()));
        assert!(model.row_data(0).is_some_and(|c| c.has_logo));
        assert!(
            model.row_data(3).is_some_and(|c| c.has_logo),
            "stesso logo, stesso indirizzo"
        );
    }

    #[test]
    fn items_have_formatted_numbers_and_stable_tints() {
        let (model, _) = model();
        let a = model.item(3).unwrap();
        assert_eq!(a.num.as_str(), "003");
        assert_eq!(a.tint, model.item(3).unwrap().tint);
        assert_eq!(format_number(1250), "1250");
    }
}
