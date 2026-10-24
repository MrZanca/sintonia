//! Modelli Slint per elenchi lunghi: canali, film, serie.
//!
//! Un solo modello generico ([`CatalogModel`]) tiene tutte le voci della sorgente (in un `Arc`,
//! senza copie) e un elenco di indici visibili, filtrato per categoria, preferiti o ricerca. Slint
//! chiede solo le righe che disegna (gli elenchi sono virtualizzati), quindi anche con decine di
//! migliaia di voci si costruiscono poche decine di righe alla volta. Le immagini (loghi o
//! locandine) si chiedono la prima volta che una riga compare.
//!
//! Le categorie hanno sempre in testa «Tutti» (indice 0) e «Preferiti» (indice 1).
//!
//! Le categorie bloccate dal PIN, finché il PIN non è stato inserito, restano nell'elenco (con il
//! lucchetto) ma le loro voci spariscono da tutto il resto: «Tutti», preferiti, ricerca, chiavi
//! (Home, ripresa, ultimo canale).

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use sintonia_core::db::{ChannelRow, MovieRow, SeriesRow};
use slint::{Color, Image, Model, ModelNotify, ModelTracker};

use crate::{CategoryItem, ChannelItem, PosterItem};

/// Indice della categoria «Preferiti».
pub const FAVORITES: usize = 1;

/// Una voce di un elenco.
pub trait CatalogEntry: 'static {
    /// La riga per Slint.
    type Item: Clone + 'static;
    /// Le immagini sono locandine (più grandi) e non loghi.
    const POSTER: bool;
    /// Identificativo nell'archivio.
    fn id(&self) -> i64;
    /// Chiave stabile (preferiti, cronologia).
    fn key(&self) -> String;
    /// Categoria.
    fn group(&self) -> &str;
    /// Indirizzo dell'immagine.
    fn image_url(&self) -> Option<&str>;
    /// La riga, con l'immagine (se pronta), il preferito e l'avanzamento.
    fn to_item(&self, image: Option<Image>, fav: bool, progress: f32) -> Self::Item;
}

/// Colori di riserva per voci senza immagine (scelti dal nome, sempre uguali).
const TINTS: [u32; 10] = [
    0x3b82f6, 0x8b5cf6, 0xef4444, 0x22c55e, 0xf59e0b, 0x14b8a6, 0xec4899, 0xa855f7, 0x0ea5e9,
    0xf97316,
];

/// Colore stabile per un nome.
pub fn tint_for(name: &str) -> Color {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    let rgb = TINTS[(hasher.finish() % TINTS.len() as u64) as usize];
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Numero del canale come lo mostra l'interfaccia ("007", "1250").
pub fn format_number(number: u32) -> String {
    format!("{number:03}")
}

impl CatalogEntry for ChannelRow {
    type Item = ChannelItem;
    const POSTER: bool = false;

    fn id(&self) -> i64 {
        self.id
    }
    fn key(&self) -> String {
        self.key.clone()
    }
    fn group(&self) -> &str {
        &self.group
    }
    fn image_url(&self) -> Option<&str> {
        self.logo.as_deref()
    }
    fn to_item(&self, image: Option<Image>, fav: bool, _progress: f32) -> ChannelItem {
        ChannelItem {
            id: i32::try_from(self.id).unwrap_or(-1),
            num: format_number(self.number).into(),
            name: self.name.as_str().into(),
            group: self.group.as_str().into(),
            has_logo: image.is_some(),
            logo: image.unwrap_or_default(),
            tint: tint_for(&self.name),
            radio: self.radio,
            drm: self.drm,
            fav,
            // la guida si aggiunge con `CatalogModel::set_decorate`
            now_progress: -1.0,
            ..ChannelItem::default()
        }
    }
}

/// Un film o una serie nella griglia.
#[derive(Debug, Clone, PartialEq)]
pub struct PosterEntry {
    /// Identificativo nell'archivio (film o serie).
    pub id: i64,
    /// Chiave stabile ("m…" o "s…").
    pub key: String,
    /// Titolo.
    pub title: String,
    /// Anno, genere…
    pub meta: String,
    /// Categoria.
    pub group: String,
    /// Locandina.
    pub cover: Option<String>,
    /// Voto su 10.
    pub rating: Option<f32>,
}

impl PosterEntry {
    /// Da un film.
    pub fn movie(m: &MovieRow) -> Self {
        Self {
            id: m.id,
            key: m.key(),
            title: m.name.clone(),
            meta: m.year.clone().unwrap_or_default(),
            group: m.group.clone(),
            cover: m.cover.clone(),
            rating: m.rating,
        }
    }

    /// Da una serie.
    pub fn series(s: &SeriesRow) -> Self {
        let meta = [s.year.as_deref(), s.genre.as_deref()]
            .into_iter()
            .flatten()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        Self {
            id: s.id,
            key: s.key(),
            title: s.name.clone(),
            meta,
            group: s.group.clone(),
            cover: s.cover.clone(),
            rating: s.rating,
        }
    }
}

/// Voto come lo mostra l'interfaccia ("7.5"): una cifra decimale, senza separatori di lingua.
pub fn format_rating(rating: Option<f32>) -> String {
    match rating {
        Some(r) if r > 0.0 => {
            let tenths = (r * 10.0).round() as i32;
            format!("{}.{}", tenths / 10, tenths % 10)
        }
        _ => String::new(),
    }
}

impl CatalogEntry for PosterEntry {
    type Item = PosterItem;
    const POSTER: bool = true;

    fn id(&self) -> i64 {
        self.id
    }
    fn key(&self) -> String {
        self.key.clone()
    }
    fn group(&self) -> &str {
        &self.group
    }
    fn image_url(&self) -> Option<&str> {
        self.cover.as_deref()
    }
    fn to_item(&self, image: Option<Image>, fav: bool, progress: f32) -> PosterItem {
        PosterItem {
            id: i32::try_from(self.id).unwrap_or(-1),
            title: self.title.as_str().into(),
            meta: self.meta.as_str().into(),
            rating: format_rating(self.rating).into(),
            has_cover: image.is_some(),
            cover: image.unwrap_or_default(),
            tint: tint_for(&self.title),
            progress,
            fav,
        }
    }
}

/// Stato dell'immagine di un indirizzo.
enum ImageState {
    Requested,
    Ready(Image),
    Missing,
}

/// Filtro attivo.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Filter {
    /// Indice nella lista delle categorie.
    Category(usize),
    /// Risultati di una ricerca (indici in `all`).
    Search(Vec<usize>),
}

/// Completa una riga con dati che non stanno nella voce.
type Decorate<E> = Box<dyn Fn(&E, &mut <E as CatalogEntry>::Item)>;

/// Il modello. Vedi la documentazione del modulo.
pub struct CatalogModel<E: CatalogEntry> {
    all: RefCell<Arc<Vec<E>>>,
    /// Indici di `all` per categoria; 0 = tutti, 1 = preferiti, poi una per categoria.
    by_category: RefCell<Vec<Vec<usize>>>,
    /// Categorie bloccate dal PIN, e se le loro voci vanno nascoste (PIN non inserito).
    locked: RefCell<HashSet<String>>,
    hide_locked: Cell<bool>,
    categories: RefCell<Vec<CategoryItem>>,
    by_id: RefCell<HashMap<i64, usize>>,
    by_key: RefCell<HashMap<String, usize>>,
    favorites: RefCell<HashSet<String>>,
    progress: RefCell<HashMap<String, f32>>,
    filter: RefCell<Filter>,
    visible: RefCell<Vec<usize>>,
    images: RefCell<HashMap<String, ImageState>>,
    request_image: Box<dyn Fn(String, bool)>,
    /// Completa una riga con dati che non stanno nella voce (la guida per i canali).
    decorate: RefCell<Option<Decorate<E>>>,
    /// Chiamata quando cambiano le righe visibili (filtro, voci).
    on_change: RefCell<Option<Box<dyn Fn()>>>,
    notify: ModelNotify,
    /// Le immagini che arrivano a raffica producono un solo aggiornamento.
    refresh_timer: slint::Timer,
    refresh_pending: Cell<bool>,
}

impl<E: CatalogEntry> std::fmt::Debug for CatalogModel<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogModel")
            .field("entries", &self.all.borrow().len())
            .field("visible", &self.visible.borrow().len())
            .finish_non_exhaustive()
    }
}

fn count_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

impl<E: CatalogEntry> CatalogModel<E> {
    /// Modello vuoto; `request_image(url, locandina)` viene chiamata per ogni immagine da
    /// scaricare.
    pub fn new(request_image: impl Fn(String, bool) + 'static) -> Rc<Self> {
        Rc::new(Self {
            all: RefCell::new(Arc::new(Vec::new())),
            by_category: RefCell::new(vec![Vec::new(), Vec::new()]),
            locked: RefCell::new(HashSet::new()),
            hide_locked: Cell::new(false),
            categories: RefCell::new(Vec::new()),
            by_id: RefCell::new(HashMap::new()),
            by_key: RefCell::new(HashMap::new()),
            favorites: RefCell::new(HashSet::new()),
            progress: RefCell::new(HashMap::new()),
            filter: RefCell::new(Filter::Category(0)),
            visible: RefCell::new(Vec::new()),
            images: RefCell::new(HashMap::new()),
            request_image: Box::new(request_image),
            decorate: RefCell::new(None),
            on_change: RefCell::new(None),
            notify: ModelNotify::default(),
            refresh_timer: slint::Timer::default(),
            refresh_pending: Cell::new(false),
        })
    }

    /// Sostituisce tutte le voci. Torna su «Tutti».
    pub fn set_entries(&self, entries: Arc<Vec<E>>) {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        let mut group_index: HashMap<String, usize> = HashMap::new();
        let mut by_id = HashMap::with_capacity(entries.len());
        let mut by_key = HashMap::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            by_id.insert(entry.id(), i);
            by_key.insert(entry.key(), i);
            let group = entry.group();
            let slot = match group_index.get(group) {
                Some(slot) => *slot,
                None => {
                    groups.push((group.to_owned(), Vec::new()));
                    group_index.insert(group.to_owned(), groups.len() - 1);
                    groups.len() - 1
                }
            };
            groups[slot].1.push(i);
        }
        // senza categoria in fondo
        groups.sort_by_key(|(name, _)| name.is_empty());
        let mut categories = Vec::with_capacity(groups.len() + 2);
        let mut by_category = Vec::with_capacity(groups.len() + 2);
        categories.push(CategoryItem {
            kind: 0,
            ..CategoryItem::default()
        });
        // «Tutti» lo riempie `rebuild_locks`
        by_category.push(Vec::new());
        categories.push(CategoryItem {
            kind: 3,
            ..CategoryItem::default()
        });
        by_category.push(Vec::new());
        for (name, indices) in groups {
            categories.push(CategoryItem {
                kind: if name.is_empty() { 2 } else { 1 },
                name: name.into(),
                count: count_i32(indices.len()),
                locked: false,
            });
            by_category.push(indices);
        }
        *self.all.borrow_mut() = entries;
        *self.by_id.borrow_mut() = by_id;
        *self.by_key.borrow_mut() = by_key;
        *self.categories.borrow_mut() = categories;
        *self.by_category.borrow_mut() = by_category;
        self.rebuild_locks();
        self.apply(Filter::Category(0));
    }

    /// Categorie bloccate dal PIN; con `hide` le loro voci spariscono (vedi il modulo). Se la
    /// categoria mostrata sparisce si torna su «Tutti».
    pub fn set_locks(&self, locked: HashSet<String>, hide: bool) {
        *self.locked.borrow_mut() = locked;
        self.hide_locked.set(hide);
        self.rebuild_locks();
        let filter = self.filter.borrow().clone();
        let filter = match filter {
            Filter::Category(i) if self.is_category_hidden(i) => Filter::Category(0),
            Filter::Search(indices) => Filter::Search(
                indices
                    .into_iter()
                    .filter(|i| !self.is_hidden(*i))
                    .collect(),
            ),
            filter => filter,
        };
        self.apply(filter);
    }

    /// La voce `index` di `all` è in una categoria nascosta.
    fn is_hidden(&self, index: usize) -> bool {
        self.hide_locked.get()
            && self
                .all
                .borrow()
                .get(index)
                .is_some_and(|e| self.locked.borrow().contains(e.group()))
    }

    /// La categoria `index` è bloccata e il PIN non è stato inserito.
    pub fn is_category_hidden(&self, index: usize) -> bool {
        self.hide_locked.get()
            && index > FAVORITES
            && self
                .categories
                .borrow()
                .get(index)
                .is_some_and(|c| c.locked)
    }

    /// «Tutti», preferiti e lucchetti delle categorie dopo un cambio di voci o di blocchi.
    fn rebuild_locks(&self) {
        let all: Vec<usize> = (0..self.all.borrow().len())
            .filter(|i| !self.is_hidden(*i))
            .collect();
        {
            let locked = self.locked.borrow();
            let mut categories = self.categories.borrow_mut();
            for category in categories.iter_mut().skip(FAVORITES + 1) {
                category.locked = locked.contains(category.name.as_str());
            }
            if let Some(first) = categories.first_mut() {
                first.count = count_i32(all.len());
            }
        }
        if let Some(slot) = self.by_category.borrow_mut().first_mut() {
            *slot = all;
        }
        self.rebuild_favorites();
    }

    /// Imposta come completare le righe (vedi `decorate`).
    pub fn set_decorate(&self, f: impl Fn(&E, &mut E::Item) + 'static) {
        *self.decorate.borrow_mut() = Some(Box::new(f));
    }

    /// Imposta chi avvisare quando cambiano le righe visibili.
    pub fn set_on_change(&self, f: impl Fn() + 'static) {
        *self.on_change.borrow_mut() = Some(Box::new(f));
    }

    /// Ricostruisce le righe visibili (per esempio quando cambia la guida).
    pub fn refresh(&self) {
        self.notify.reset();
    }

    /// Chiavi dei preferiti (di questo tipo di voce).
    pub fn set_favorites(&self, keys: HashSet<String>) {
        *self.favorites.borrow_mut() = keys;
        self.rebuild_favorites();
        let filter = self.filter.borrow().clone();
        self.apply(filter);
    }

    fn rebuild_favorites(&self) {
        let favorites = self.favorites.borrow();
        let all = self.all.borrow();
        let indices: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(i, e)| favorites.contains(&e.key()) && !self.is_hidden(*i))
            .map(|(i, _)| i)
            .collect();
        if let Some(category) = self.categories.borrow_mut().get_mut(FAVORITES) {
            category.count = count_i32(indices.len());
        }
        if let Some(slot) = self.by_category.borrow_mut().get_mut(FAVORITES) {
            *slot = indices;
        }
    }

    /// Avanzamento (0..1) per chiave.
    pub fn set_progress(&self, progress: HashMap<String, f32>) {
        *self.progress.borrow_mut() = progress;
        self.notify.reset();
    }

    /// La chiave è tra i preferiti.
    pub fn is_favorite(&self, key: &str) -> bool {
        self.favorites.borrow().contains(key)
    }

    /// Le categorie, con «Tutti» e «Preferiti» in testa.
    pub fn categories(&self) -> Vec<CategoryItem> {
        self.categories.borrow().clone()
    }

    /// Mostra una categoria.
    pub fn show_category(&self, index: usize) {
        let index = index.min(self.by_category.borrow().len().saturating_sub(1));
        self.apply(Filter::Category(index));
    }

    /// Mostra i risultati di una ricerca (identificativi, nell'ordine dell'elenco).
    pub fn show_search(&self, ids: &[i64]) {
        let by_id = self.by_id.borrow();
        let indices = ids
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .filter(|i| !self.is_hidden(*i))
            .collect();
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
            Filter::Category(i) if self.is_category_hidden(*i) => Vec::new(),
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
        if let Some(on_change) = self.on_change.borrow().as_ref() {
            on_change();
        }
    }

    /// La voce alla riga `row` dell'elenco visibile.
    pub fn entry_at(&self, row: usize) -> Option<(i64, String)> {
        let index = *self.visible.borrow().get(row)?;
        self.all.borrow().get(index).map(|e| (e.id(), e.key()))
    }

    /// Riga dell'elenco visibile che contiene la voce `id`.
    pub fn row_of(&self, id: i64) -> Option<usize> {
        let index = *self.by_id.borrow().get(&id)?;
        self.visible.borrow().iter().position(|i| *i == index)
    }

    /// Esegue `f` sulla voce `id`.
    pub fn with_entry<R>(&self, id: i64, f: impl FnOnce(&E) -> R) -> Option<R> {
        let index = *self.by_id.borrow().get(&id)?;
        self.all.borrow().get(index).map(f)
    }

    /// Identificativo della voce con chiave `key` (`None` anche se è nascosta dal PIN).
    pub fn id_of_key(&self, key: &str) -> Option<i64> {
        let index = *self.by_key.borrow().get(key)?;
        if self.is_hidden(index) {
            return None;
        }
        self.all.borrow().get(index).map(CatalogEntry::id)
    }

    /// La voce `id` è nascosta dal PIN.
    pub fn is_id_hidden(&self, id: i64) -> bool {
        let index = self.by_id.borrow().get(&id).copied();
        index.is_some_and(|index| self.is_hidden(index))
    }

    /// La riga per Slint della voce `id`.
    pub fn item(&self, id: i64) -> Option<E::Item> {
        let index = *self.by_id.borrow().get(&id)?;
        self.item_at_index(index)
    }

    /// Prima voce (non nascosta).
    pub fn first_id(&self) -> Option<i64> {
        let first = *self.by_category.borrow().first()?.first()?;
        self.all.borrow().get(first).map(CatalogEntry::id)
    }

    /// Nessuna voce.
    pub fn is_empty(&self) -> bool {
        self.all.borrow().is_empty()
    }

    fn item_at_index(&self, index: usize) -> Option<E::Item> {
        let all = self.all.borrow();
        let entry = all.get(index)?;
        let key = entry.key();
        let image = entry.image_url().and_then(|url| self.image(url));
        let fav = self.favorites.borrow().contains(&key);
        let progress = self.progress.borrow().get(&key).copied().unwrap_or(0.0);
        let mut item = entry.to_item(image, fav, progress);
        if let Some(decorate) = self.decorate.borrow().as_ref() {
            decorate(entry, &mut item);
        }
        Some(item)
    }

    /// Immagine pronta, oppure la chiede (una volta sola).
    pub fn image(&self, url: &str) -> Option<Image> {
        let mut images = self.images.borrow_mut();
        match images.get(url) {
            Some(ImageState::Ready(image)) => Some(image.clone()),
            Some(ImageState::Requested | ImageState::Missing) => None,
            None => {
                images.insert(url.to_owned(), ImageState::Requested);
                drop(images);
                (self.request_image)(url.to_owned(), E::POSTER);
                None
            }
        }
    }

    /// Un'immagine è arrivata (o non è disponibile).
    pub fn image_ready(self: &Rc<Self>, url: String, image: Option<Image>) {
        let state = image.map_or(ImageState::Missing, ImageState::Ready);
        self.images.borrow_mut().insert(url, state);
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
                    // con gli elenchi virtualizzati si ricalcolano solo le righe visibili
                    model.notify.reset();
                }
            },
        );
    }
}

impl<E: CatalogEntry> Model for CatalogModel<E> {
    type Data = E::Item;

    fn row_count(&self) -> usize {
        self.visible.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<E::Item> {
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
            key: format!("k{id}"),
            number: u32::try_from(id).unwrap_or(0),
            name: name.into(),
            group: group.into(),
            logo: logo.map(Into::into),
            ..ChannelRow::default()
        }
    }

    type Requested = Rc<RefCell<Vec<(String, bool)>>>;

    fn model() -> (Rc<CatalogModel<ChannelRow>>, Requested) {
        let requested: Requested = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&requested);
        let model = CatalogModel::new(move |url, poster| sink.borrow_mut().push((url, poster)));
        model.set_entries(Arc::new(vec![
            row(1, "Uno", "News", Some("http://l/1.png")),
            row(2, "Due", "", None),
            row(3, "Tre", "Sport", Some("http://l/3.png")),
            row(4, "Quattro", "News", Some("http://l/1.png")),
        ]));
        (model, requested)
    }

    #[test]
    fn categories_start_with_all_and_favorites() {
        let (model, _) = model();
        let cats = model.categories();
        let names: Vec<(&str, i32, i32)> = cats
            .iter()
            .map(|c| (c.name.as_str(), c.count, c.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("", 4, 0),
                ("", 0, 3),
                ("News", 2, 1),
                ("Sport", 1, 1),
                ("", 1, 2)
            ]
        );
    }

    #[test]
    fn favorites_filter_and_flag() {
        let (model, _) = model();
        model.set_favorites(HashSet::from(["k3".to_owned()]));
        assert_eq!(model.categories()[FAVORITES].count, 1);
        model.show_category(FAVORITES);
        assert_eq!(model.row_count(), 1);
        assert!(model.row_data(0).is_some_and(|c| c.fav && c.name == "Tre"));
        // togliendo il preferito mentre si guarda la categoria, l'elenco si aggiorna
        model.set_favorites(HashSet::new());
        assert_eq!(model.row_count(), 0);
    }

    #[test]
    fn category_and_search_filters() {
        let (model, _) = model();
        assert_eq!(model.row_count(), 4);
        model.show_category(2);
        assert_eq!(model.row_count(), 2);
        assert_eq!(model.entry_at(1), Some((4, "k4".to_owned())));
        assert_eq!(model.row_of(4), Some(1));
        assert_eq!(model.row_of(2), None);
        model.show_search(&[3, 99, 1]);
        assert_eq!(model.row_count(), 2);
        assert_eq!(model.entry_at(0).map(|e| e.0), Some(3));
        model.show_category(42);
        assert_eq!(
            model.row_count(),
            1,
            "indice fuori misura: ultima categoria"
        );
        assert_eq!(model.id_of_key("k2"), Some(2));
    }

    #[test]
    fn locked_categories_hide_their_entries() {
        let (model, _) = model();
        model.set_favorites(HashSet::from(["k1".to_owned(), "k3".to_owned()]));
        model.show_category(2);
        // News bloccata, PIN non inserito
        model.set_locks(HashSet::from(["News".to_owned()]), true);
        assert_eq!(model.category(), 0, "la categoria mostrata sparisce");
        assert_eq!(model.row_count(), 2);
        let cats = model.categories();
        assert_eq!(cats[0].count, 2);
        assert_eq!(cats[FAVORITES].count, 1);
        assert!(cats[2].locked && !cats[3].locked);
        assert!(model.is_category_hidden(2) && !model.is_category_hidden(3));
        model.show_category(2);
        assert_eq!(model.row_count(), 0);
        assert_eq!(model.id_of_key("k1"), None);
        assert!(model.is_id_hidden(4));
        model.show_search(&[1, 3]);
        assert_eq!(model.row_count(), 1);
        // PIN inserito: tutto torna, il lucchetto resta
        model.set_locks(HashSet::from(["News".to_owned()]), false);
        assert_eq!(model.categories()[0].count, 4);
        assert!(model.categories()[2].locked);
        assert_eq!(model.id_of_key("k1"), Some(1));
        // si può bloccare anche «senza categoria»
        model.set_locks(HashSet::from([String::new()]), true);
        assert!(model.is_category_hidden(4));
        assert_eq!(model.first_id(), Some(1));
        model.set_locks(HashSet::from(["News".to_owned()]), true);
        assert_eq!(model.first_id(), Some(2));
    }

    #[test]
    fn images_are_requested_once_per_url() {
        let (model, requested) = model();
        for row in 0..model.row_count() {
            let _ = model.row_data(row);
        }
        let _ = model.row_data(0);
        assert_eq!(
            *requested.borrow(),
            [
                ("http://l/1.png".to_owned(), false),
                ("http://l/3.png".to_owned(), false)
            ]
        );
        model.image_ready("http://l/1.png".into(), Some(Image::default()));
        assert!(model.row_data(0).is_some_and(|c| c.has_logo));
        assert!(model.row_data(3).is_some_and(|c| c.has_logo));
    }

    #[test]
    fn posters_and_ratings() {
        let movie = MovieRow {
            id: 5,
            xtream_id: 77,
            name: "Film".into(),
            year: Some("2020".into()),
            rating: Some(7.25),
            ..MovieRow::default()
        };
        let entry = PosterEntry::movie(&movie);
        assert_eq!(entry.key, "m77");
        let item = entry.to_item(None, true, 0.5);
        assert_eq!(item.rating.as_str(), "7.3");
        assert_eq!(item.meta.as_str(), "2020");
        assert!(item.fav && !item.has_cover);
        assert_eq!(format_rating(None), "");
        assert_eq!(format_number(1250), "1250");
        let series = SeriesRow {
            id: 1,
            xtream_id: 9,
            name: "S".into(),
            year: Some("2001".into()),
            genre: Some("Dramma".into()),
            ..SeriesRow::default()
        };
        assert_eq!(PosterEntry::series(&series).meta, "2001 · Dramma");
    }
}
