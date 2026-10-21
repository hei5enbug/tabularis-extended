export interface TableColumn {
  name: string;
  data_type: string;
  is_pk: boolean;
  primary_key_position?: number | null;
  is_nullable: boolean;
  is_auto_increment: boolean;
  is_generated?: boolean;
  character_maximum_length?: number;
  comment?: string | null;
}

export interface ForeignKey {
  name: string;
  column_name: string;
  ref_table: string;
  ref_column: string;
}

export interface Index {
  name: string;
  column_name: string;
  is_unique: boolean;
  is_primary: boolean;
  seq_in_index?: number;
  is_expression?: boolean;
}
