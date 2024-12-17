use inflector::Inflector;
use proc_macro::{self, TokenStream};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{
    parse_macro_input, Attribute, Data, DataStruct, DeriveInput, Field, Fields, FieldsNamed, Ident,
    LitStr,
};

#[proc_macro_derive(SqlxCrud, attributes(database, table, external_id, id))]
pub fn derive(input: TokenStream) -> TokenStream {
    let DeriveInput {
        ident, data, attrs, ..
    } = parse_macro_input!(input);
    match data {
        Data::Struct(DataStruct {
            fields: Fields::Named(FieldsNamed { named, .. }),
            ..
        }) => {
            let config = Config::new(&attrs, &ident, &named);
            let static_model_schema = build_static_model_schema(&config);
            let sqlx_crud_impl = build_sqlx_crud_impl(&config);

            quote! {
                #static_model_schema
                #sqlx_crud_impl
            }
            .into()
        }
        _ => panic!("this derive macro only works on structs with named fields"),
    }
}

fn build_static_model_schema(config: &Config) -> TokenStream2 {
    let crate_name = &config.crate_name;
    let model_schema_ident = &config.model_schema_ident;
    let table_name = &config.table_name;

    let id_column = config.id_column_ident.to_string();
    let columns_len = config.named.iter().count();
    let columns = config
        .named
        .iter()
        .flat_map(|f| &f.ident)
        .map(|f| LitStr::new(format!("{}", f).as_str(), f.span()));

    let sql_queries = build_sql_queries(config);

    quote! {
        #[automatically_derived]
        static #model_schema_ident: #crate_name::schema::Metadata<'static, #columns_len> = #crate_name::schema::Metadata {
            table_name: #table_name,
            id_column: #id_column,
            columns: [#(#columns),*],
            #sql_queries
        };
    }
}

fn build_sql_queries(config: &Config) -> TokenStream2 {
    let table_name = config.quote_ident(&config.table_name);
    let id_column = format!(
        "{}.{}",
        &table_name,
        config.quote_ident(&config.id_column_ident.to_string())
    );

    let insert_bind_cnt = if config.external_id {
        config.named.iter().count()
    } else {
        config.named.iter().count() - 1
    };
    let insert_sql_binds = (0..insert_bind_cnt)
        .map(|i| format!("${}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");

    let update_sql_binds = config
        .named
        .iter()
        .flat_map(|f| &f.ident)
        .filter(|i| *i != &config.id_column_ident)
        .enumerate()
        .map(|(i, ident)| format!("{} = ${}", config.quote_ident(&ident.to_string()), i + 1))
        .collect::<Vec<_>>();

    let insert_column_list = config
        .named
        .iter()
        .flat_map(|f| &f.ident)
        .filter(|i| config.external_id || *i != &config.id_column_ident)
        .map(|i| config.quote_ident(&i.to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    let column_list = config
        .named
        .iter()
        .flat_map(|f| &f.ident)
        .map(|i| format!("{}.{}", &table_name, config.quote_ident(&i.to_string())))
        .collect::<Vec<_>>()
        .join(", ");

    let select_sql = format!("SELECT {} FROM {}", column_list, table_name);
    let paginated_sql = format!(
        "SELECT {} FROM {} LIMIT $1 OFFSET $2",
        column_list, table_name
    );
    let select_by_id_sql = format!(
        "SELECT {} FROM {} WHERE {} = $1 LIMIT 1",
        column_list, table_name, id_column
    );
    let insert_sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) RETURNING {}",
        table_name, insert_column_list, insert_sql_binds, column_list
    );
    let update_by_id_sql = format!(
        "UPDATE {} SET {} WHERE {} = ${} RETURNING {}",
        table_name,
        update_sql_binds.join(", "),
        id_column,
        update_sql_binds.len() + 1,
        column_list
    );
    let delete_by_id_sql = format!("DELETE FROM {} WHERE {} = $1", table_name, id_column);

    quote! {
        select_sql: #select_sql,
        select_by_id_sql: #select_by_id_sql,
        insert_sql: #insert_sql,
        update_by_id_sql: #update_by_id_sql,
        delete_by_id_sql: #delete_by_id_sql,
        paginated_sql: #paginated_sql,
    }
}

fn build_sqlx_crud_impl(config: &Config) -> TokenStream2 {
    let crate_name = &config.crate_name;
    let ident = &config.ident;
    let model_schema_ident = &config.model_schema_ident;
    let db_ty = config.db_ty.sqlx_db();
    let id_column_ident = &config.id_column_ident;

    let id_ty = config
        .named
        .iter()
        .find(|f| f.ident.as_ref() == Some(id_column_ident))
        .map(|f| &f.ty)
        .expect("the id type");

    let insert_query_args = config
        .named
        .iter()
        .flat_map(|f| &f.ident)
        .filter(|i| config.external_id || *i != &config.id_column_ident)
        .map(|i| quote! { let _ = args.add(self.#i); });

    let insert_query_size = config
        .named
        .iter()
        .flat_map(|f| &f.ident)
        .filter(|i| config.external_id || *i != &config.id_column_ident)
        .map(|i| quote! { ::sqlx::encode::Encode::<#db_ty>::size_hint(&self.#i) });

    let update_query_args = config
        .named
        .iter()
        .flat_map(|f| &f.ident)
        .filter(|i| *i != &config.id_column_ident)
        .map(|i| quote! { let _ = args.add(self.#i); });

    let update_query_args_id = quote! { let _ = args.add(self.#id_column_ident); };

    let update_query_size = config
        .named
        .iter()
        .flat_map(|f| &f.ident)
        .map(|i| quote! { ::sqlx::encode::Encode::<#db_ty>::size_hint(&self.#i) });

    quote! {
        #[automatically_derived]
        impl #crate_name::traits::Schema for #ident {
            type Id = #id_ty;

            fn table_name() -> &'static str {
                #model_schema_ident.table_name
            }

            fn id(&self) -> Self::Id {
                self.#id_column_ident
            }

            fn id_column() -> &'static str {
                #model_schema_ident.id_column
            }

            fn columns() -> &'static [&'static str] {
                &#model_schema_ident.columns
            }

            fn select_sql() -> &'static str {
                #model_schema_ident.select_sql
            }

            fn select_by_id_sql() -> &'static str {
                #model_schema_ident.select_by_id_sql
            }

            fn insert_sql() -> &'static str {
                #model_schema_ident.insert_sql
            }

            fn update_by_id_sql() -> &'static str {
                #model_schema_ident.update_by_id_sql
            }

            fn delete_by_id_sql() -> &'static str {
                #model_schema_ident.delete_by_id_sql
            }

            fn paginated_sql() -> &'static str {
                #model_schema_ident.paginated_sql
            }
        }

        #[automatically_derived]
        impl<'e> #crate_name::traits::Crud<'e, &'e ::sqlx::pool::Pool<#db_ty>> for #ident {
            fn insert_args(self) -> <#db_ty as ::sqlx::database::Database>::Arguments<'e> {
                use ::sqlx::Arguments as _;
                let mut args = <#db_ty as ::sqlx::database::Database>::Arguments::default();
                args.reserve(1usize, #(#insert_query_size)+*);
                #(#insert_query_args)*
                args
            }

            fn update_args(self) -> <#db_ty as ::sqlx::database::Database>::Arguments<'e> {
                use ::sqlx::Arguments as _;
                let mut args = <#db_ty as ::sqlx::database::Database>::Arguments::default();
                args.reserve(1usize, #(#update_query_size)+*);
                #(#update_query_args)*
                #update_query_args_id
                args
            }

            fn paginated_args(limit: i64, offset: i64) -> <#db_ty as ::sqlx::database::Database>::Arguments<'e> {
                use ::sqlx::Arguments as _;
                let mut args = <#db_ty as ::sqlx::database::Database>::Arguments::default();
                args.reserve(2usize,
                    ::sqlx::encode::Encode::<#db_ty>::size_hint(&limit) +
                    ::sqlx::encode::Encode::<#db_ty>::size_hint(&offset)
                );
                let _ = args.add(limit);
                let _ = args.add(offset);
                args
            }
        }
    }
}

#[allow(dead_code)] // Usage in quote macros aren't flagged as used
struct Config<'a> {
    ident: &'a Ident,
    named: &'a Punctuated<Field, Comma>,
    crate_name: TokenStream2,
    db_ty: DbType,
    model_schema_ident: Ident,
    table_name: String,
    id_column_ident: Ident,
    external_id: bool,
}

impl<'a> Config<'a> {
    fn new(attrs: &[Attribute], ident: &'a Ident, named: &'a Punctuated<Field, Comma>) -> Self {
        let crate_name = std::env::var("CARGO_PKG_NAME").unwrap();
        let is_doctest = std::env::vars()
            .any(|(k, _)| k == "UNSTABLE_RUSTDOC_TEST_LINE" || k == "UNSTABLE_RUSTDOC_TEST_PATH");
        let crate_name = if !is_doctest && crate_name == "sqlx-crud" {
            quote! { crate }
        } else {
            quote! { ::sqlx_crud }
        };

        let db_ty = DbType::new(attrs);

        let model_schema_ident =
            format_ident!("{}_SCHEMA", ident.to_string().to_screaming_snake_case());

        // Search for a field with the #[id] attribute
        let id_attr = &named
            .iter()
            .find(|f| f.attrs.iter().any(|a| a.path().is_ident("id")))
            .and_then(|f| f.ident.as_ref());
        // Otherwise default to the first field as the "id" column
        let id_column_ident = id_attr
            .unwrap_or_else(|| {
                named
                    .iter()
                    .flat_map(|f| &f.ident)
                    .next()
                    .expect("the first field")
            })
            .clone();

        let external_id = attrs.iter().any(|a| a.path().is_ident("external_id"));

        let table_name = attrs
            .iter()
            .find(|a| a.path().is_ident("table"))
            .and_then(|attr| {
                let mut table = None;
                attr.parse_nested_meta(|meta| {
                    if let Some(ident) = meta.path.get_ident() {
                        table = Some(ident.to_string());
                    }
                    Ok(())
                })
                .ok();
                table
            })
            .unwrap_or_else(|| ident.to_string().to_table_case());

        Self {
            ident,
            named,
            crate_name,
            db_ty,
            model_schema_ident,
            table_name,
            id_column_ident,
            external_id,
        }
    }

    fn quote_ident(&self, ident: &str) -> String {
        self.db_ty.quote_ident(ident)
    }
}

enum DbType {
    Any,
    Mssql,
    MySql,
    Postgres,
    Sqlite,
}

impl From<&str> for DbType {
    fn from(db_type: &str) -> Self {
        match db_type {
            "Any" => Self::Any,
            "Mssql" => Self::Mssql,
            "MySql" => Self::MySql,
            "Postgres" => Self::Postgres,
            "Sqlite" => Self::Sqlite,
            _ => panic!("unknown #[database] type {}", db_type),
        }
    }
}

impl DbType {
    fn new(attrs: &[Attribute]) -> Self {
        let mut db_type = DbType::Sqlite;
        attrs
            .iter()
            .find(|a| a.path().is_ident("database"))
            .map(|a| {
                a.parse_nested_meta(|m| {
                    if let Some(path) = m.path.get_ident() {
                        db_type = DbType::from(path.to_string().as_str());
                    }
                    Ok(())
                })
            });

        db_type
    }

    fn sqlx_db(&self) -> TokenStream2 {
        match self {
            Self::Any => quote! { ::sqlx::Any },
            Self::Mssql => quote! { ::sqlx::Mssql },
            Self::MySql => quote! { ::sqlx::MySql },
            Self::Postgres => quote! { ::sqlx::Postgres },
            Self::Sqlite => quote! { ::sqlx::Sqlite },
        }
    }

    fn quote_ident(&self, ident: &str) -> String {
        match self {
            Self::Any => format!(r#""{}""#, &ident),
            Self::Mssql => format!(r#""{}""#, &ident),
            Self::MySql => format!("`{}`", &ident),
            Self::Postgres => format!(r#""{}""#, &ident),
            Self::Sqlite => format!(r#""{}""#, &ident),
        }
    }
}
