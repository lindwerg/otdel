//! What a page is, before anybody tries to read it: geometry, orientation, images.
//!
//! This pass touches the object graph only — it never decodes a content stream, so it is
//! cheap and it cannot be derailed by a font the text parser dislikes. That matters,
//! because the inventory is what guarantees the promise "all 32 + 12 pages are accounted
//! for": the page rows exist before the first page is read, so an interruption halfway
//! through leaves 20 pages honestly `pending`, not 20 pages missing.

use pdf_extract::{Dictionary, Document, Object, ObjectId};

use crate::error::{ExtractError, ExtractResult};
use crate::model::{DocumentInventory, PageInventory};

/// US Letter in points — used only when a page declares no media box at all, and
/// recorded as such is better than storing nothing for a page that does exist.
const FALLBACK_SIZE: (f64, f64) = (612.0, 792.0);

/// How far up the page tree an inherited attribute is followed.
const MAX_INHERITANCE_DEPTH: usize = 32;

/// Enumerate the pages of a document.
pub(crate) fn document_inventory(
    doc: &Document,
    max_pages: u32,
) -> ExtractResult<DocumentInventory> {
    if doc.is_encrypted() {
        return Err(ExtractError::Encrypted);
    }

    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err(ExtractError::NoPages);
    }

    let page_count = u32::try_from(pages.len()).unwrap_or(u32::MAX);
    if page_count > max_pages {
        return Err(ExtractError::TooManyPages {
            found: page_count,
            limit: max_pages,
        });
    }

    let pages = pages
        .into_iter()
        .map(|(page_number, page_id)| page_inventory(doc, page_number, page_id))
        .collect::<Vec<_>>();

    Ok(DocumentInventory { page_count, pages })
}

/// Facts about a single page. Never fails: a page whose dictionary is unreadable is
/// still a page, and is reported with fallback geometry rather than dropped.
pub(crate) fn page_inventory(doc: &Document, page_number: u32, page_id: ObjectId) -> PageInventory {
    let (width_pt, height_pt) = media_box(doc, page_id).unwrap_or(FALLBACK_SIZE);
    PageInventory {
        page_number,
        width_pt,
        height_pt,
        rotation: rotation(doc, page_id),
        image_count: image_count(doc, page_id),
    }
}

fn media_box(doc: &Document, page_id: ObjectId) -> Option<(f64, f64)> {
    let object = inherited(doc, page_id, b"MediaBox")?;
    let values = object.as_array().ok()?;
    if values.len() != 4 {
        return None;
    }
    let numbers: Vec<f64> = values.iter().filter_map(number).collect();
    if numbers.len() != 4 {
        return None;
    }
    let width = (numbers[2] - numbers[0]).abs();
    let height = (numbers[3] - numbers[1]).abs();
    (width > 0.0 && height > 0.0 && width.is_finite() && height.is_finite())
        .then_some((width, height))
}

/// `/Rotate`, normalised to 0/90/180/270 — the database only accepts those, and a PDF
/// may legally write `-90` or `450`.
fn rotation(doc: &Document, page_id: ObjectId) -> i32 {
    let Some(object) = inherited(doc, page_id, b"Rotate") else {
        return 0;
    };
    let Ok(raw) = object.as_i64() else {
        return 0;
    };
    let normalised = raw.rem_euclid(360);
    match normalised {
        90 | 180 | 270 => normalised as i32,
        _ => 0,
    }
}

/// Image XObjects reachable from the page's resources.
///
/// Counted, not decoded: the number is only ever used to tell "scan" from "blank page".
fn image_count(doc: &Document, page_id: ObjectId) -> u32 {
    let Ok((inline, referenced)) = doc.get_page_resources(page_id) else {
        return 0;
    };

    let mut total = 0u32;
    if let Some(dictionary) = inline {
        total = total.saturating_add(count_images_in_resources(doc, dictionary));
    }
    for id in referenced {
        if let Ok(dictionary) = doc.get_dictionary(id) {
            total = total.saturating_add(count_images_in_resources(doc, dictionary));
        }
    }
    total
}

fn count_images_in_resources(doc: &Document, resources: &Dictionary) -> u32 {
    let Ok(xobjects) = resources
        .get_deref(b"XObject", doc)
        .and_then(Object::as_dict)
    else {
        return 0;
    };

    xobjects
        .iter()
        .filter(|(_, object)| {
            let resolved = doc
                .dereference(object)
                .map(|(_, resolved)| resolved)
                .unwrap_or(object);
            resolved
                .as_stream()
                .ok()
                .and_then(|stream| stream.dict.get(b"Subtype").ok())
                .and_then(|subtype| subtype.as_name().ok())
                .is_some_and(|name| name == b"Image")
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Look up an attribute on the page, following `/Parent` for the inheritable ones.
fn inherited<'a>(doc: &'a Document, page_id: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut current = page_id;
    for _ in 0..MAX_INHERITANCE_DEPTH {
        let dictionary = doc.get_dictionary(current).ok()?;
        if let Ok(object) = dictionary.get_deref(key, doc) {
            if !matches!(object, Object::Null) {
                return Some(object);
            }
        }
        current = dictionary.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}
