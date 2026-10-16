use serde::{Deserialize, Serialize};

/// A curated template providing sane defaults for a well-known service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceTemplate {
    /// Template name (e.g., "redis", "postgres").
    pub name: &'static str,
    /// Human description.
    pub description: &'static str,
    /// Package name in nixpkgs (e.g., "valkey" for redis).
    pub package: &'static str,
    /// Command to start the service.
    pub command: &'static str,
    /// Default command-line arguments.
    pub args: &'static str,
    /// Default port.
    pub port: u16,
    /// Port protocol (tcp/unix).
    pub protocol: &'static str,
    /// Port label for the ports table key.
    pub port_label: &'static str,
    /// Data directory relative to $HOME/.local/share/.
    pub data_dir: &'static str,
    /// Health check command (if any).
    pub health_check: Option<&'static str>,
    /// Shutdown signal.
    pub stop_signal: &'static str,
}

/// Built-in service templates.
pub fn all_templates() -> &'static [ServiceTemplate] {
    &[
        ServiceTemplate {
            name: "redis",
            description: "Redis-compatible in-memory data store (valkey)",
            package: "valkey",
            command: "valkey-server",
            args: "--port 6379 --bind 127.0.0.1 --daemonize no",
            port: 6379,
            protocol: "tcp",
            port_label: "redis",
            data_dir: "redis",
            health_check: Some("valkey-cli ping"),
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "postgres",
            description: "PostgreSQL relational database",
            package: "postgresql",
            command: "postgres",
            args: "-D $HOME/.local/share/postgres/data -k /tmp",
            port: 5432,
            protocol: "tcp",
            port_label: "postgres",
            data_dir: "postgres",
            health_check: Some("pg_isready -h 127.0.0.1 -p 5432"),
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "mysql",
            description: "MySQL/MariaDB relational database",
            package: "mariadb",
            command: "mariadbd",
            args: "--datadir=$HOME/.local/share/mysql/data --socket=/tmp/mysql.sock --port=3306 \
                   --bind-address=127.0.0.1",
            port: 3306,
            protocol: "tcp",
            port_label: "mysql",
            data_dir: "mysql",
            health_check: Some("mariadb-admin ping -h 127.0.0.1 -P 3306"),
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "mongodb",
            description: "MongoDB document database",
            package: "mongodb",
            command: "mongod",
            args: "--dbpath $HOME/.local/share/mongodb/data --bind_ip 127.0.0.1 --port 27017",
            port: 27017,
            protocol: "tcp",
            port_label: "mongodb",
            data_dir: "mongodb",
            health_check: None,
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "rabbitmq",
            description: "RabbitMQ message broker",
            package: "rabbitmq-server",
            command: "rabbitmq-server",
            args: "",
            port: 5672,
            protocol: "tcp",
            port_label: "amqp",
            data_dir: "rabbitmq",
            health_check: Some("rabbitmq-diagnostics ping"),
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "nats",
            description: "NATS messaging system",
            package: "nats-server",
            command: "nats-server",
            args: "-a 127.0.0.1 -p 4222 --store_dir $HOME/.local/share/nats/data",
            port: 4222,
            protocol: "tcp",
            port_label: "nats",
            data_dir: "nats",
            health_check: None,
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "minio",
            description: "MinIO S3-compatible object storage",
            package: "minio",
            command: "minio",
            args: "server $HOME/.local/share/minio/data --address 127.0.0.1:9000 \
                   --console-address 127.0.0.1:9001",
            port: 9000,
            protocol: "tcp",
            port_label: "s3",
            data_dir: "minio",
            health_check: None,
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "nginx",
            description: "nginx HTTP server",
            package: "nginx",
            command: "nginx",
            args: "-g 'daemon off;' -c $HOME/.local/share/nginx/nginx.conf",
            port: 8080,
            protocol: "tcp",
            port_label: "http",
            data_dir: "nginx",
            health_check: None,
            stop_signal: "SIGQUIT",
        },
        ServiceTemplate {
            name: "caddy",
            description: "Caddy web server",
            package: "caddy",
            command: "caddy",
            args: "run --config $HOME/.local/share/caddy/Caddyfile",
            port: 8080,
            protocol: "tcp",
            port_label: "http",
            data_dir: "caddy",
            health_check: None,
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "memcached",
            description: "Memcached in-memory cache",
            package: "memcached",
            command: "memcached",
            args: "-l 127.0.0.1 -p 11211",
            port: 11211,
            protocol: "tcp",
            port_label: "memcached",
            data_dir: "memcached",
            health_check: None,
            stop_signal: "SIGTERM",
        },
        ServiceTemplate {
            name: "clickhouse",
            description: "ClickHouse analytics database",
            package: "clickhouse",
            command: "clickhouse-server",
            args: "--config-file=$HOME/.local/share/clickhouse/config.xml",
            port: 8123,
            protocol: "tcp",
            port_label: "http",
            data_dir: "clickhouse",
            health_check: None,
            stop_signal: "SIGTERM",
        },
    ]
}

/// Look up a template by name.
pub fn find_template(name: &str) -> Option<&'static ServiceTemplate> {
    all_templates().iter().find(|t| t.name == name)
}

/// Apply a template to a TOML config table, setting defaults for
/// keys that are not already present. Returns the service name to use
/// in the manifest (the template's package name).
pub fn apply_template(template: &ServiceTemplate, config: &mut toml::Table) {
    if !config.contains_key("command") {
        config.insert(
            "command".to_owned(),
            toml::Value::String(template.command.to_owned()),
        );
    }
    if !config.contains_key("args") && !template.args.is_empty() {
        config.insert(
            "args".to_owned(),
            toml::Value::String(template.args.to_owned()),
        );
    }
    if !config.contains_key("description") {
        config.insert(
            "description".to_owned(),
            toml::Value::String(template.description.to_owned()),
        );
    }

    if !config.contains_key("stop_signal") {
        config.insert(
            "stop_signal".to_owned(),
            toml::Value::String(template.stop_signal.to_owned()),
        );
    }
    if let Some(hc) = template.health_check {
        if !config.contains_key("health_check") {
            config.insert(
                "health_check".to_owned(),
                toml::Value::String(hc.to_owned()),
            );
        }
    }

    // Set up ports table.
    if !config.contains_key("ports") {
        let mut port_entry = toml::Table::new();
        port_entry.insert(
            "port".to_owned(),
            toml::Value::Integer(i64::from(template.port)),
        );
        port_entry.insert(
            "protocol".to_owned(),
            toml::Value::String(template.protocol.to_owned()),
        );

        let mut ports = toml::Table::new();
        ports.insert(
            template.port_label.to_owned(),
            toml::Value::Table(port_entry),
        );
        config.insert("ports".to_owned(), toml::Value::Table(ports));
    }
}

/// List template names for shell completion.
pub fn template_names() -> Vec<String> {
    all_templates().iter().map(|t| t.name.to_owned()).collect()
}
