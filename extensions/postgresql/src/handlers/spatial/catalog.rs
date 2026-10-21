use serde::Serialize;
use tokio_postgres::{Client, Column};

use super::{ewkb, SpatialError};
use crate::utils::identifiers::quote_identifier;

#[derive(Clone, Debug)]
pub struct Catalog {
    pub schema: String,
    pub geometry_oid: u32,
    pub geography_oid: u32,
}

impl Catalog {
    pub async fn load(client: &Client) -> Result<Option<Self>, SpatialError> {
        let row = client.query_opt(
            "SELECT n.nspname::text, \
             (SELECT t.oid FROM pg_type t JOIN pg_depend d ON d.objid=t.oid AND d.classid='pg_type'::regclass \
              WHERE d.refobjid=e.oid AND d.refclassid='pg_extension'::regclass AND d.deptype='e' AND t.typname='geometry') AS geometry_oid, \
             (SELECT t.oid FROM pg_type t JOIN pg_depend d ON d.objid=t.oid AND d.classid='pg_type'::regclass \
              WHERE d.refobjid=e.oid AND d.refclassid='pg_extension'::regclass AND d.deptype='e' AND t.typname='geography') AS geography_oid \
             FROM pg_extension e JOIN pg_namespace n ON n.oid=e.extnamespace WHERE e.extname='postgis'", &[])
            .await.map_err(SpatialError::database)?;
        row.map(|r| {
            Ok(Self {
                schema: r.get(0),
                geometry_oid: r.try_get(1).map_err(|_| {
                    SpatialError::unsupported("PostGIS geometry type is unavailable")
                })?,
                geography_oid: r.try_get(2).map_err(|_| {
                    SpatialError::unsupported("PostGIS geography type is unavailable")
                })?,
            })
        })
        .transpose()
    }
    pub fn function(&self, name: &str) -> String {
        format!(
            "{}.{}",
            quote_identifier(&self.schema),
            quote_identifier(name)
        )
    }
    pub fn native_type(&self, ty: &tokio_postgres::types::Type) -> Option<&'static str> {
        let ty = ewkb::base_type(ty);
        if ty.schema() != self.schema {
            return None;
        }
        if ty.oid() == self.geometry_oid {
            Some("geometry")
        } else if ty.oid() == self.geography_oid {
            Some("geography")
        } else {
            None
        }
    }
    fn spatial_array(&self, ty: &tokio_postgres::types::Type) -> bool {
        match ewkb::base_type(ty).kind() {
            tokio_postgres::types::Kind::Array(element) => {
                self.native_type(element).is_some() || self.spatial_array(element)
            }
            _ => false,
        }
    }
    pub fn validate_columns(&self, columns: &[Column]) -> Result<(), SpatialError> {
        for column in columns {
            let ty = ewkb::base_type(column.type_());
            if self.spatial_array(ty) {
                return Err(SpatialError::unsupported("Spatial arrays are unsupported"));
            }
            if let Some(native) = self.native_type(ty) {
                let oid = if native == "geometry" {
                    self.geometry_oid
                } else {
                    self.geography_oid
                };
                if ty.oid() != oid || ty.schema() != self.schema {
                    return Err(SpatialError::unsupported(
                        "Type is not owned by the PostGIS extension",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SpatialColumn {
    #[serde(rename = "name")]
    pub column: String,
    pub column_index: usize,
    pub native_type: String,
    pub type_oid: u32,
    pub base_oid: u32,
    pub typmod: i32,
    pub nullable: bool,
    pub srid: Option<i32>,
    pub srid_status: String,
    pub dimensions: Option<String>,
    pub geometry_type: Option<String>,
    pub primary_key: Vec<String>,
    pub spatial_index: bool,
}

#[derive(Clone, Debug)]
pub struct TableCatalog {
    pub columns: Vec<SpatialColumn>,
    pub primary_key: Vec<String>,
    pub all_columns: Vec<String>,
    pub column_types: std::collections::HashMap<String, String>,
    pub keyset_supported: bool,
}

fn supports_keyset(oid: u32) -> bool {
    use tokio_postgres::types::Type;
    [
        Type::BOOL,
        Type::INT2,
        Type::INT4,
        Type::INT8,
        Type::FLOAT4,
        Type::FLOAT8,
        Type::NUMERIC,
        Type::TEXT,
        Type::VARCHAR,
        Type::BPCHAR,
        Type::NAME,
        Type::UUID,
        Type::DATE,
        Type::TIME,
        Type::TIMESTAMP,
        Type::TIMESTAMPTZ,
        Type::OID,
        Type::INET,
        Type::CIDR,
        Type::MACADDR,
        Type::MACADDR8,
        Type::BIT,
        Type::VARBIT,
    ]
    .iter()
    .any(|ty| ty.oid() == oid)
}

pub async fn table_columns(
    client: &Client,
    catalog: &Catalog,
    schema: &str,
    table: &str,
) -> Result<TableCatalog, SpatialError> {
    let relation = client
        .query_opt(
            "SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
         WHERE n.nspname=$1 AND c.relname=$2 AND c.relkind IN ('r','p','v','m','f')",
            &[&schema, &table],
        )
        .await
        .map_err(SpatialError::database)?
        .ok_or_else(|| SpatialError::argument("Table does not exist"))?;
    let relation_oid: u32 = relation.get(0);
    let primary_key = client
        .query(
            "SELECT a.attname::text FROM pg_constraint c \
         CROSS JOIN LATERAL unnest(c.conkey) WITH ORDINALITY AS k(attnum,ord) \
         JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=k.attnum \
         WHERE c.conrelid=$1 AND c.contype='p' ORDER BY k.ord",
            &[&relation_oid],
        )
        .await
        .map_err(SpatialError::database)?
        .iter()
        .map(|r| r.get(0))
        .collect::<Vec<String>>();
    let query = format!(
        "WITH RECURSIVE ty AS ( \
         SELECT a.attnum,a.attname,a.atttypid AS type_oid,a.atttypid AS base_oid,a.atttypmod AS typmod,a.attnotnull,0 AS depth \
         FROM pg_attribute a WHERE a.attrelid=$1 AND a.attnum>0 AND NOT a.attisdropped \
         UNION ALL SELECT ty.attnum,ty.attname,ty.type_oid,t.typbasetype, \
         CASE WHEN ty.typmod<0 THEN t.typtypmod ELSE ty.typmod END,ty.attnotnull OR t.typnotnull,ty.depth+1 \
         FROM ty JOIN pg_type t ON t.oid=ty.base_oid WHERE t.typtype='d' AND ty.depth<64) \
         SELECT ty.attname::text,ty.type_oid,ty.base_oid,ty.typmod,ty.attnotnull,t.typname::text, \
         pg_catalog.format_type(ty.base_oid,ty.typmod)::text AS data_type, \
         CASE WHEN ty.base_oid IN ($2,$3) AND ty.typmod>=0 THEN {}(ty.typmod) ELSE 0 END AS srid, \
         CASE WHEN ty.base_oid IN ($2,$3) AND ty.typmod>=0 THEN {}(ty.typmod) ELSE NULL END AS geometry_type, \
         gc.coord_dimension::integer AS geometry_dimensions,gg.coord_dimension::integer AS geography_dimensions, \
         EXISTS (SELECT 1 FROM pg_index i JOIN pg_class ic ON ic.oid=i.indexrelid \
                 JOIN pg_am am ON am.oid=ic.relam WHERE i.indrelid=$1 AND i.indisvalid \
                 AND am.amname IN ('gist','spgist','brin') AND ty.attnum=ANY(i.indkey)) AS spatial_index \
         FROM ty JOIN pg_type t ON t.oid=ty.base_oid \
         LEFT JOIN {}.geometry_columns gc ON gc.f_table_schema=$4 AND gc.f_table_name=$5 AND gc.f_geometry_column=ty.attname \
         LEFT JOIN {}.geography_columns gg ON gg.f_table_schema=$4 AND gg.f_table_name=$5 AND gg.f_geography_column=ty.attname \
         WHERE t.typtype<>'d' OR ty.depth=64 ORDER BY ty.attnum",
        catalog.function("postgis_typmod_srid"), catalog.function("postgis_typmod_type"),
        quote_identifier(&catalog.schema), quote_identifier(&catalog.schema));
    let rows = client
        .query(
            &query,
            &[
                &relation_oid,
                &catalog.geometry_oid,
                &catalog.geography_oid,
                &schema,
                &table,
            ],
        )
        .await
        .map_err(SpatialError::database)?;
    let mut columns = Vec::new();
    let mut all_columns = Vec::new();
    let mut column_types = std::collections::HashMap::new();
    let mut keyset_supported = true;
    for (column_index, row) in rows.into_iter().enumerate() {
        let name: String = row.get(0);
        all_columns.push(name.clone());
        column_types.insert(name.clone(), row.get(6));
        let oid: u32 = row.get(2);
        if primary_key.contains(&name) && !supports_keyset(oid) {
            keyset_supported = false;
        }
        if oid != catalog.geometry_oid && oid != catalog.geography_oid {
            continue;
        }
        let srid: i32 = row.get(7);
        let typmod: i32 = row.get(3);
        let geometry_type: Option<String> = row.get(8);
        let dimensions = geometry_type.as_deref().map(|g| {
            if g.ends_with("ZM") {
                "XYZM"
            } else if g.ends_with('Z') {
                "XYZ"
            } else if g.ends_with('M') {
                "XYM"
            } else {
                "XY"
            }
            .to_string()
        });
        columns.push(SpatialColumn {
            column: name,
            column_index,
            native_type: row.get(5),
            type_oid: row.get(1),
            base_oid: oid,
            typmod,
            nullable: !row.get::<_, bool>(4),
            srid: (srid != 0).then_some(srid),
            srid_status: if srid != 0 {
                "known"
            } else if typmod < 0 {
                "mixed"
            } else {
                "unknown"
            }
            .into(),
            dimensions,
            geometry_type,
            primary_key: primary_key.clone(),
            spatial_index: row.get(11),
        });
    }
    Ok(TableCatalog {
        columns,
        primary_key,
        all_columns,
        column_types,
        keyset_supported,
    })
}
