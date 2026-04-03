use tokio_postgres::{Client, NoTls};

use crate::cli::{ConnectionArgs, SslMode};
use crate::error::CliError;

/// Establish a connection to PostgreSQL using the resolved connection parameters.
///
/// Resolution order:
/// 1. `--url` / `-U` CLI argument (highest priority)
/// 2. `PGTRICKLE_URL` environment variable
/// 3. Individual `--host`/`--port`/`--dbname`/`--user`/`--password` flags
///    (which also read from PGHOST/PGPORT/PGDATABASE/PGUSER/PGPASSWORD)
/// 4. Defaults: localhost:5432/postgres
pub async fn connect(args: &ConnectionArgs) -> Result<Client, CliError> {
    let conn_string = build_connection_string(args);

    match args.sslmode {
        SslMode::Disable => connect_no_tls(&conn_string).await,
        SslMode::Prefer => {
            // Try TLS first, fall back to plain
            match connect_with_tls(&conn_string, &args.sslrootcert).await {
                Ok(client) => Ok(client),
                Err(_) => connect_no_tls(&conn_string).await,
            }
        }
        SslMode::Require => connect_with_tls(&conn_string, &args.sslrootcert).await,
    }
}

async fn connect_no_tls(conn_string: &str) -> Result<Client, CliError> {
    let (client, connection) = tokio_postgres::connect(conn_string, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {e}");
        }
    });
    Ok(client)
}

async fn connect_with_tls(
    conn_string: &str,
    _sslrootcert: &Option<String>,
) -> Result<Client, CliError> {
    // For TLS we require the tokio-postgres-rustls feature.
    // When not compiled with TLS support, fall back to NoTls.
    #[cfg(feature = "tls")]
    {
        use std::sync::Arc;
        use tokio_postgres_rustls::MakeRustlsConnect;

        let mut root_store = rustls::RootCertStore::empty();

        if let Some(ref cert_path) = _sslrootcert {
            // Load custom CA certificate
            let cert_data = std::fs::read(cert_path)
                .map_err(|e| CliError::ConnectionMsg(format!("reading SSL cert: {e}")))?;
            let certs = rustls_pemfile::certs(&mut &cert_data[..])
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();
            for cert in certs {
                root_store
                    .add(cert)
                    .map_err(|e| CliError::ConnectionMsg(format!("adding SSL cert: {e}")))?;
            }
        } else {
            // Use system certificates
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }

        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let tls = MakeRustlsConnect::new(Arc::new(tls_config));

        let (client, connection) = tokio_postgres::connect(conn_string, tls).await?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("TLS connection error: {e}");
            }
        });
        Ok(client)
    }
    #[cfg(not(feature = "tls"))]
    {
        // TLS not compiled in — fall back to NoTls
        connect_no_tls(conn_string).await
    }
}

pub fn build_connection_string(args: &ConnectionArgs) -> String {
    if let Some(url) = &args.url {
        return url.clone();
    }

    let host = args.host.as_deref().unwrap_or("localhost");
    let port = args.port.unwrap_or(5432);
    let dbname = args.dbname.as_deref().unwrap_or("postgres");
    let user = args.user.as_deref().unwrap_or("postgres");

    let mut conn = format!("host={host} port={port} dbname={dbname} user={user}");

    if let Some(password) = &args.password {
        // Use the key=value format which handles special characters.
        conn.push_str(&format!(" password={password}"));
    }

    conn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ThemeChoice;

    fn test_args() -> ConnectionArgs {
        ConnectionArgs {
            url: None,
            host: None,
            port: None,
            dbname: None,
            user: None,
            password: None,
            interval: 2,
            sslmode: SslMode::Disable,
            sslrootcert: None,
            theme: ThemeChoice::Dark,
            mouse: false,
            bell: false,
        }
    }

    #[test]
    fn test_default_connection_string() {
        let args = test_args();
        let s = build_connection_string(&args);
        assert_eq!(s, "host=localhost port=5432 dbname=postgres user=postgres");
    }

    #[test]
    fn test_url_takes_precedence() {
        let mut args = test_args();
        args.url = Some("postgres://myhost:9999/mydb".to_string());
        args.host = Some("other".to_string());
        args.port = Some(1234);
        let s = build_connection_string(&args);
        assert_eq!(s, "postgres://myhost:9999/mydb");
    }

    #[test]
    fn test_individual_params() {
        let mut args = test_args();
        args.host = Some("db.example.com".to_string());
        args.port = Some(5433);
        args.dbname = Some("mydb".to_string());
        args.user = Some("admin".to_string());
        args.password = Some("secret".to_string());
        let s = build_connection_string(&args);
        assert_eq!(
            s,
            "host=db.example.com port=5433 dbname=mydb user=admin password=secret"
        );
    }
}
