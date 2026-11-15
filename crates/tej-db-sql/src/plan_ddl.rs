use crate::constraint::{CheckConstraint, ForeignKeyConstraint, ReferentialAction};
use crate::plan::map_sql_data_type;
use crate::schema::{ColumnDef, TableSchema};
use sqlparser::ast::{
    ColumnOption, CreateTable, ReferentialAction as SqlReferentialAction, TableConstraint,
};
use tej_db_core::Result;

/// Plans a `CREATE TABLE` AST statement into a validated `TableSchema`.
pub fn plan_create_table(ct: CreateTable) -> Result<TableSchema> {
    let table_name = ct.name.to_string();
    let mut col_defs = Vec::with_capacity(ct.columns.len());

    let mut table_pks = Vec::new();
    let mut foreign_keys = Vec::new();
    let mut check_constraints = Vec::new();

    for constraint in ct.constraints {
        match constraint {
            TableConstraint::PrimaryKey { columns, .. } => {
                for col in columns {
                    table_pks.push(col.to_string());
                }
            }
            TableConstraint::ForeignKey {
                name,
                columns,
                foreign_table,
                referred_columns,
                on_delete,
                on_update,
                ..
            } => {
                let cols = columns.into_iter().map(|c| c.value).collect();
                let f_table = foreign_table.to_string();
                let f_cols = referred_columns.into_iter().map(|c| c.value).collect();
                let on_del = map_referential_action(on_delete);
                let on_upd = map_referential_action(on_update);
                let mut fk = ForeignKeyConstraint::new(cols, f_table, f_cols, on_del, on_upd);
                fk.name = name.map(|n| n.value);
                foreign_keys.push(fk);
            }
            TableConstraint::Check { name, expr } => {
                let mut check = CheckConstraint::new(expr.to_string());
                check.name = name.map(|n| n.value);
                check_constraints.push(check);
            }
            _ => {}
        }
    }

    for col in ct.columns {
        let c_name = col.name.value;
        let data_type = map_sql_data_type(&col.data_type)?;
        let mut is_pk = table_pks.iter().any(|pk| pk.eq_ignore_ascii_case(&c_name));
        let mut is_nullable = true;

        for opt in col.options {
            match opt.option {
                ColumnOption::Unique { is_primary, .. } => {
                    if is_primary {
                        is_pk = true;
                        is_nullable = false;
                    }
                }
                ColumnOption::NotNull => {
                    is_nullable = false;
                }
                ColumnOption::Check(expr) => {
                    check_constraints.push(CheckConstraint::new(expr.to_string()));
                }
                ColumnOption::ForeignKey {
                    foreign_table,
                    referred_columns,
                    on_delete,
                    on_update,
                    ..
                } => {
                    let cols = vec![c_name.clone()];
                    let f_table = foreign_table.to_string();
                    let f_cols = referred_columns.into_iter().map(|c| c.value).collect();
                    let on_del = map_referential_action(on_delete);
                    let on_upd = map_referential_action(on_update);
                    foreign_keys.push(ForeignKeyConstraint::new(
                        cols, f_table, f_cols, on_del, on_upd,
                    ));
                }
                _ => {}
            }
        }

        col_defs.push(ColumnDef {
            name: c_name,
            data_type,
            is_primary_key: is_pk,
            is_nullable,
        });
    }

    Ok(TableSchema::new_with_constraints(
        table_name,
        col_defs,
        foreign_keys,
        check_constraints,
    ))
}

fn map_referential_action(action: Option<SqlReferentialAction>) -> ReferentialAction {
    match action {
        Some(SqlReferentialAction::Cascade) => ReferentialAction::Cascade,
        Some(SqlReferentialAction::SetNull) => ReferentialAction::SetNull,
        Some(SqlReferentialAction::Restrict) => ReferentialAction::Restrict,
        Some(SqlReferentialAction::SetDefault) => ReferentialAction::SetDefault,
        _ => ReferentialAction::NoAction,
    }
}
