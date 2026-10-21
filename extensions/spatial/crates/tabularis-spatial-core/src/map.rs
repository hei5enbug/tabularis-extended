use serde::{Deserialize, Serialize};
use crate::{CoreError, ErrorCode, LayerSource, Viewport, MAX_SAFE_INTEGER};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Basemap { pub style_url: String, pub attribution: String }

impl Basemap {
    pub fn validate(&self) -> Result<(), CoreError> {
        let https = self.style_url.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"));
        let authority = self.style_url.get(8..).unwrap_or_default().split(['/', '?', '#']).next().unwrap_or_default();
        if !https || authority.is_empty() || authority.contains('@') || self.style_url.chars().count() > 2048
            || self.attribution.chars().count() > 4096 || self.style_url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') {
            return Err(invalid("Basemap requires an HTTPS URL without user information and bounded plain attribution"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BasemapChange { #[default] Keep, Set(Basemap), Clear }

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointStyle {
    #[serde(skip_serializing_if = "Option::is_none")] pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub radius: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineStyle {
    #[serde(skip_serializing_if = "Option::is_none")] pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub width: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolygonStyle {
    #[serde(skip_serializing_if = "Option::is_none")] pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub opacity: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Style {
    #[serde(skip_serializing_if = "Option::is_none")] pub point: Option<PointStyle>,
    #[serde(skip_serializing_if = "Option::is_none")] pub line: Option<LineStyle>,
    #[serde(skip_serializing_if = "Option::is_none")] pub polygon: Option<PolygonStyle>,
}

impl Style {
    pub fn validate(&self) -> Result<(), CoreError> {
        if let Some(point) = &self.point {
            color_and_opacity(&point.color, point.opacity)?;
            number_range(point.radius, 1.0, 40.0)?;
        }
        if let Some(line) = &self.line {
            color_and_opacity(&line.color, line.opacity)?;
            number_range(line.width, 1.0, 20.0)?;
        }
        if let Some(polygon) = &self.polygon { color_and_opacity(&polygon.color, polygon.opacity)?; }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultReference { pub result_id: String, pub result_set_index: u64 }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureRef { pub layer_id: String, pub feature_id: String }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerState {
    pub layer_id: String,
    pub connection_id: String,
    pub source: LayerSource,
    pub visible: bool,
    pub style: Style,
    pub generation: u64,
    pub result_references: Vec<ResultReference>,
}

impl LayerState {
    pub fn new(layer_id: impl Into<String>, connection_id: impl Into<String>, source: LayerSource, style: Style) -> Result<Self, CoreError> {
        let result_references = references_for(&source);
        let layer = Self { layer_id: layer_id.into(), connection_id: connection_id.into(), source, visible: true, style, generation: 0, result_references };
        layer.validate()?;
        Ok(layer)
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        identifier(&self.layer_id)?;
        identifier(&self.connection_id)?;
        self.source.validate()?;
        self.style.validate()?;
        safe_counter(self.generation)?;
        let mut seen = std::collections::HashSet::new();
        for reference in &self.result_references {
            identifier(&reference.result_id)?;
            safe_counter(reference.result_set_index)?;
            if !seen.insert((&reference.result_id, reference.result_set_index)) { return Err(invalid("Duplicate result reference")); }
        }
        Ok(())
    }

    pub fn ensure_generation(&self, generation: u64) -> Result<(), CoreError> {
        if generation != self.generation { return Err(CoreError::new(ErrorCode::StaleGeneration, "Layer generation has changed")); }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapState {
    pub map_id: String,
    pub name: String,
    pub version: u64,
    pub viewport: Viewport,
    pub basemap: Option<Basemap>,
    pub layers: Vec<LayerState>,
    pub selected_feature_refs: Vec<FeatureRef>,
}

impl MapState {
    pub fn new(map_id: impl Into<String>, name: impl Into<String>) -> Result<Self, CoreError> {
        let map = Self { map_id: map_id.into(), name: name.into(), version: 0, viewport: Viewport::default(), basemap: None, layers: Vec::new(), selected_feature_refs: Vec::new() };
        map.validate()?;
        Ok(map)
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        identifier(&self.map_id)?;
        if self.name.is_empty() { return Err(invalid("Map name must be nonempty")); }
        safe_counter(self.version)?;
        self.viewport.validate()?;
        if let Some(basemap) = &self.basemap { basemap.validate()?; }
        let mut layers = std::collections::HashSet::new();
        for layer in &self.layers {
            layer.validate()?;
            if !layers.insert(&layer.layer_id) { return Err(invalid("Layer IDs must be unique")); }
        }
        let mut selection = std::collections::HashSet::new();
        for reference in &self.selected_feature_refs {
            if reference.feature_id.is_empty() || !layers.contains(&reference.layer_id)
                || !selection.insert((&reference.layer_id, &reference.feature_id)) {
                return Err(invalid("Selection requires unique references to existing layers"));
            }
        }
        Ok(())
    }

    pub fn update(&mut self, expected_version: u64, name: Option<String>, basemap: BasemapChange) -> Result<u64, CoreError> {
        self.mutate(expected_version, |map| {
            if name.is_none() && basemap == BasemapChange::Keep { return Err(invalid("Map update requires a change")); }
            if let Some(name) = name { map.name = name; }
            match basemap { BasemapChange::Keep => {}, BasemapChange::Set(basemap) => map.basemap = Some(basemap), BasemapChange::Clear => map.basemap = None }
            Ok(())
        })
    }

    pub fn add_layer(&mut self, expected_version: u64, layer: LayerState) -> Result<u64, CoreError> {
        self.mutate(expected_version, |map| { map.layers.push(layer); Ok(()) })
    }

    pub fn remove_layer(&mut self, expected_version: u64, layer_id: &str) -> Result<u64, CoreError> {
        self.mutate(expected_version, |map| {
            let index = map.layer_index(layer_id)?;
            map.layers.remove(index);
            map.selected_feature_refs.retain(|reference| reference.layer_id != layer_id);
            Ok(())
        })
    }

    pub fn update_layer(&mut self, expected_version: u64, layer_id: &str, visible: Option<bool>, style: Option<Style>) -> Result<u64, CoreError> {
        self.mutate(expected_version, |map| {
            if visible.is_none() && style.is_none() { return Err(invalid("Layer update requires a change")); }
            let index = map.layer_index(layer_id)?;
            let layer = &mut map.layers[index];
            if let Some(visible) = visible { layer.visible = visible; }
            if let Some(style) = style { layer.style = style; }
            layer.generation = increment(layer.generation)?;
            Ok(())
        })
    }

    pub fn replace_layer_source(&mut self, expected_version: u64, layer_id: &str, source: LayerSource) -> Result<u64, CoreError> {
        self.mutate(expected_version, |map| {
            let index = map.layer_index(layer_id)?;
            let layer = &mut map.layers[index];
            layer.result_references = references_for(&source);
            layer.source = source;
            layer.generation = increment(layer.generation)?;
            Ok(())
        })
    }

    pub fn set_viewport(&mut self, expected_version: u64, viewport: Viewport) -> Result<u64, CoreError> {
        self.mutate(expected_version, |map| {
            map.viewport = viewport;
            for layer in &mut map.layers {
                if let LayerSource::Table { viewport: source_viewport, .. } = &mut layer.source {
                    *source_viewport = viewport;
                    layer.generation = increment(layer.generation)?;
                }
            }
            Ok(())
        })
    }

    pub fn set_selection(&mut self, expected_version: u64, references: Vec<FeatureRef>) -> Result<u64, CoreError> {
        self.mutate(expected_version, |map| { map.selected_feature_refs = references; Ok(()) })
    }

    fn mutate(&mut self, expected_version: u64, mutation: impl FnOnce(&mut Self) -> Result<(), CoreError>) -> Result<u64, CoreError> {
        if expected_version != self.version { return Err(CoreError::version_conflict(self.version)); }
        let version = increment(self.version)?;
        let mut candidate = self.clone();
        mutation(&mut candidate)?;
        candidate.version = version;
        candidate.validate()?;
        *self = candidate;
        Ok(version)
    }

    fn layer_index(&self, layer_id: &str) -> Result<usize, CoreError> {
        self.layers.iter().position(|layer| layer.layer_id == layer_id).ok_or_else(|| CoreError::new(ErrorCode::NotFound, "Layer does not exist"))
    }
}

fn references_for(source: &LayerSource) -> Vec<ResultReference> {
    match source { LayerSource::QueryResult { result_id, result_set_index, .. } => vec![ResultReference { result_id: result_id.clone(), result_set_index: *result_set_index }], LayerSource::Table { .. } => Vec::new() }
}

fn color_and_opacity(color: &Option<String>, opacity: Option<f64>) -> Result<(), CoreError> {
    if let Some(color) = color {
        if !(color.len() == 7 || color.len() == 9) || !color.starts_with('#') || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit) {
            return Err(invalid("Style color must be a six or eight digit hex color"));
        }
    }
    number_range(opacity, 0.0, 1.0)
}

fn number_range(value: Option<f64>, minimum: f64, maximum: f64) -> Result<(), CoreError> {
    if value.is_some_and(|value| !value.is_finite() || !(minimum..=maximum).contains(&value)) { return Err(invalid("Style value is outside its allowed range")); }
    Ok(())
}

fn identifier(value: &str) -> Result<(), CoreError> {
    if value.is_empty() || value.chars().count() > 128 { return Err(invalid("Identifier must contain 1 to 128 characters")); }
    Ok(())
}

fn safe_counter(value: u64) -> Result<(), CoreError> {
    if value > MAX_SAFE_INTEGER { return Err(invalid("Counter exceeds the JSON safe integer range")); }
    Ok(())
}

fn increment(value: u64) -> Result<u64, CoreError> {
    value.checked_add(1).filter(|next| *next <= MAX_SAFE_INTEGER).ok_or_else(|| CoreError::new(ErrorCode::ResourceLimit, "Counter cannot be incremented safely"))
}

fn invalid(message: &str) -> CoreError { CoreError::new(ErrorCode::InvalidArgument, message) }
