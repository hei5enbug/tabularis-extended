// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;

use crate::{
    core::TdsResult, datatypes::sqltypes::SqlType, io::packet_writer::PacketWriter,
    token::tokens::SqlCollation,
};

#[async_trait]
pub(crate) trait SqlValueEncoder {
    async fn encode_sqlvalue(
        &self,
        packet_writer: &mut PacketWriter<'_>,
        sql_value: &SqlType,
        db_collation: &SqlCollation,
    ) -> TdsResult<()>;
}

pub struct GenericEncoder {}

impl GenericEncoder {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl SqlValueEncoder for GenericEncoder {
    async fn encode_sqlvalue(
        &self,
        packet_writer: &mut PacketWriter<'_>,
        sql_value: &SqlType,
        db_collation: &SqlCollation,
    ) -> TdsResult<()> {
        sql_value.serialize(packet_writer, db_collation).await?;
        Ok(())
    }
}
