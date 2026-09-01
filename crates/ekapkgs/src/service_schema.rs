//! Service option schema types and cache.
//!
//! Consumes the JSON produced by `generate-service-options.nix` and caches it
//! as a zstd-compressed index at `~/.cache/ekapkgs/indexes/service-options.json.zst`.
//!
//! The schema describes the configuration surface for arbitrary service
//! definitions. Two tiers:
//!
//! - **`base`** — the universal service interface (common + platform options). Valid for any
//!   service, even ones the module system has never seen.
//!
//! - **`services`** — per-service schemas from an evaluated configuration, including custom
//!   extensions (`settings.*`, extra options, etc.).
//!
//! The CLI uses `services.<name>` when available and falls back to `base`.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;

use ekapkgs_nix::NixCommand;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Schema types — mirror the JSON produced by generate-service-options.nix
// ---------------------------------------------------------------------------

/// Top-level schema output from the nix generator.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServiceOptionsSchema {
    /// Universal service interface options (valid for any service).
    pub base: BaseSchema,
    /// Per-service schemas discovered from a configuration evaluation.
    #[serde(default)]
    pub services: HashMap<String, ServiceDef>,
}

/// The base (universal) service option set.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BaseSchema {
    pub options: Vec<OptionDef>,
}

/// A discovered service with its full option tree.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServiceDef {
    pub description: String,
    pub options: Vec<OptionDef>,
}

/// A single option declaration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OptionDef {
    /// Dot-separated option path (e.g. `settings.permitRootLogin`).
    pub path: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Structured type information.
    #[serde(rename = "type")]
    pub option_type: OptionType,
    /// Whether this option must be set (has no default).
    #[serde(default)]
    pub required: bool,
    /// Default value as a JSON string, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Example value as a JSON string, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
}

/// Structured type representation using tagged unions.
///
/// The `kind` field determines which extra fields are present.
/// The CLI can pattern-match on this to render appropriate input widgets.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind")]
pub enum OptionType {
    #[serde(rename = "bool")]
    Bool,
    #[serde(rename = "str")]
    Str,
    #[serde(rename = "int")]
    Int {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        positive: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unsigned: bool,
    },
    #[serde(rename = "port")]
    Port,
    #[serde(rename = "path")]
    Path,
    #[serde(rename = "lines")]
    Lines,
    #[serde(rename = "float")]
    Float,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "package")]
    Package,
    #[serde(rename = "enum")]
    Enum { values: Vec<serde_json::Value> },
    #[serde(rename = "listOf")]
    ListOf {
        element: Box<OptionType>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        non_empty: bool,
    },
    #[serde(rename = "attrsOf")]
    AttrsOf { element: Box<OptionType> },
    #[serde(rename = "nullOr")]
    NullOr { inner: Box<OptionType> },
    #[serde(rename = "either")]
    Either { variants: Vec<OptionType> },
    #[serde(rename = "submodule")]
    Submodule { options: Vec<OptionDef> },
    #[serde(rename = "anything")]
    Anything,
    #[serde(rename = "unspecified")]
    Unspecified {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Cache infrastructure
// ---------------------------------------------------------------------------

const INDEX_NAME: &str = "service-options";

fn cache_dir() -> color_eyre::Result<PathBuf> {
    let dir = directories::ProjectDirs::from("", "", "ekapkgs")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache/ekapkgs")
        });
    let index_dir = dir.join("indexes");
    std::fs::create_dir_all(&index_dir)?;
    Ok(index_dir)
}

fn index_path() -> color_eyre::Result<PathBuf> {
    Ok(cache_dir()?.join(format!("{INDEX_NAME}.json.zst")))
}

/// Write the schema to the cache as zstd-compressed JSON.
pub fn write_cache(schema: &ServiceOptionsSchema) -> color_eyre::Result<()> {
    let data = serde_json::to_vec(schema)?;
    let path = index_path()?;
    let compressed = zstd::encode_all(data.as_slice(), 3)?;
    std::fs::write(&path, compressed)?;
    tracing::info!(
        "Wrote service schema cache {} ({} bytes)",
        path.display(),
        data.len()
    );
    Ok(())
}

/// Read the cached schema, if it exists.
pub fn read_cache() -> color_eyre::Result<Option<ServiceOptionsSchema>> {
    let path = index_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let compressed = std::fs::read(&path)?;
    let mut decoder = zstd::Decoder::new(compressed.as_slice())?;
    let mut data = Vec::new();
    decoder.read_to_end(&mut data)?;
    let schema: ServiceOptionsSchema = serde_json::from_slice(&data)?;
    Ok(Some(schema))
}

// ---------------------------------------------------------------------------
// Generation — invoke nix to produce the schema
// ---------------------------------------------------------------------------

/// Generate the service options schema by evaluating the nix generator.
///
/// `flake` is the flake reference to evaluate (e.g. `.` for the current
/// directory, or a full flake URL). The generator is embedded as an inline
/// nix expression that imports the service module infrastructure from the
/// flake's pkgs.
pub fn generate(flake: &str) -> color_eyre::Result<ServiceOptionsSchema> {
    // Canonicalize local flake references (`.`, `./path`) to absolute paths
    // since `builtins.getFlake` requires absolute paths for local flakes.
    let flake_ref = if flake == "." || flake.starts_with("./") || flake.starts_with('/') {
        let path = std::path::Path::new(flake);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        abs.canonicalize()
            .unwrap_or(abs)
            .to_string_lossy()
            .into_owned()
    } else {
        flake.to_owned()
    };

    // Inline nix expression that:
    // 1. Imports the flake's pkgs
    // 2. Imports generate-service-options.nix from the ekaos lib
    // 3. Evaluates and returns the schema as JSON
    //
    // The expression is self-contained — it finds generate-service-options.nix
    // relative to the ekaos directory in the flake.
    let expr = format!(
        r#"
        let
          flake = builtins.getFlake "{flake_ref}";
          system = builtins.currentSystem;
          pkgs = flake.legacyPackages.${{system}}
                 or flake.pkgs.${{system}}
                 or (import <nixpkgs> {{}});
          lib = pkgs.lib;

          serviceLib = import (flake.outPath + "/services/lib/service-module.nix") {{ inherit lib pkgs; }};

          # Base schema: introspect the canonical service type
          serviceEval = lib.evalModules {{
            modules = [{{ options.services = serviceLib.mkServicesOption; }}];
          }};
          servicesType = serviceEval.options.services.type;
          serviceSubOpts = servicesType.getSubOptions [];

          # Evaluated schema: discover per-service options from the flake config.
          # Try multiple strategies to find service option declarations:
          #   1. flake.ekaosConfigurations.<first> — explicit ekaos config output
          #   2. flake.config — pre-evaluated config output
          #   3. ekaos/eval-config.nix — evaluate the module system directly
          #   4. Empty fallback
          hasEvalConfig = builtins.pathExists (flake.outPath + "/ekaos/eval-config.nix");
          configEval =
            if builtins.hasAttr "ekaosConfigurations" flake then
              let
                configs = flake.ekaosConfigurations;
                firstName = builtins.head (builtins.attrNames configs);
              in configs.${{firstName}}
            else if builtins.hasAttr "config" flake then
              flake
            else if hasEvalConfig then
              (import (flake.outPath + "/ekaos/eval-config.nix") {{ inherit lib pkgs; }}) {{ modules = []; }}
            else
              {{ options = {{ services = {{}}; }}; }};

          evalServicesOptions = configEval.options.services or {{}};

          # --- type introspection (same logic as generate-service-options.nix) ---
          typeToSchema = depth: type:
            if depth > 8 then {{ kind = "unspecified"; }}
            else let
              name = type.name or "unspecified";
              next = depth + 1;
            in
              if name == "bool" then {{ kind = "bool"; }}
              else if name == "str" || name == "string" then {{ kind = "str"; }}
              else if name == "int" || name == "integer" then {{ kind = "int"; }}
              else if name == "positiveInt" then {{ kind = "int"; positive = true; }}
              else if name == "unsignedInt" then {{ kind = "int"; unsigned = true; }}
              else if name == "unsignedInt16" then {{ kind = "port"; }}
              else if name == "path" then {{ kind = "path"; }}
              else if name == "separatedString" then {{ kind = "lines"; }}
              else if name == "float" then {{ kind = "float"; }}
              else if name == "number" then {{ kind = "number"; }}
              else if name == "package" then {{ kind = "package"; }}
              else if name == "anything" || name == "unspecified" || name == "raw" then {{ kind = "anything"; }}
              else if name == "enum" then
                let
                  payload = type.functor.payload or {{}};
                  values = payload.values or [];
                  safeValues = builtins.filter (v: builtins.isString v || builtins.isInt v || builtins.isBool v) values;
                in {{ kind = "enum"; values = safeValues; }}
              else if name == "listOf" || name == "nonEmptyListOf" then
                let elem = type.nestedTypes.elemType or null;
                in {{ kind = "listOf"; element = if elem != null then typeToSchema next elem else {{ kind = "unspecified"; }}; }}
                  // lib.optionalAttrs (name == "nonEmptyListOf") {{ nonEmpty = true; }}
              else if name == "attrsOf" || name == "lazyAttrsOf" then
                let elem = type.nestedTypes.elemType or null;
                in {{ kind = "attrsOf"; element = if elem != null then typeToSchema next elem else {{ kind = "unspecified"; }}; }}
              else if name == "nullOr" then
                let elem = type.nestedTypes.elemType or null;
                in {{ kind = "nullOr"; inner = if elem != null then typeToSchema next elem else {{ kind = "unspecified"; }}; }}
              else if name == "either" then
                let
                  left = type.nestedTypes.left or null;
                  right = type.nestedTypes.right or null;
                  flattenEither = t: let n = t.name or ""; in
                    if n == "either" then
                      (flattenEither (t.nestedTypes.left or t)) ++ (flattenEither (t.nestedTypes.right or t))
                    else [ (typeToSchema next t) ];
                  variants = (if left != null then flattenEither left else [])
                    ++ (if right != null then flattenEither right else []);
                in {{ kind = "either"; inherit variants; }}
              else if name == "submodule" then
                let
                  subOpts = type.getSubOptions [];
                  identity = x: x;
                  options = optionsToSchemaList (depth + 1) identity subOpts;
                in {{ kind = "submodule"; inherit options; }}
              else if lib.hasPrefix "signedInt" name || lib.hasPrefix "unsignedInt" name || lib.hasPrefix "positiveInt" name
              then {{ kind = "int"; }}
              else {{ kind = "unspecified"; description = type.description or name; }};

          isModuleInternal = path: lib.hasPrefix "_module." path || lib.hasPrefix "_freeformOptions." path;

          isDrv = v: builtins.isAttrs v && (v._type or "" == "derivation" || builtins.hasAttr "outPath" v);

          safeToJSON = v:
            if v == null then "null"
            else if isDrv v then
              let nr = builtins.tryEval (v.pname or v.name or "<package>");
              in builtins.toJSON (if nr.success then "<${{nr.value}}>" else "<package>")
            else let r = builtins.tryEval (builtins.unsafeDiscardStringContext (builtins.toJSON v));
              in if r.success then r.value else null;

          safeEval = v: let r = builtins.tryEval v; in if r.success then r.value else null;

          optionToSchema = depth: stripFn: opt:
            let
              optName = lib.showOption opt.loc;
              visible = opt.visible or true;
              internal = opt.internal or false;
              path = stripFn optName;
              skip = internal || (builtins.isBool visible && !visible) || isModuleInternal path;
              optType = opt.type or null;
              typeSchema = if optType != null then typeToSchema depth optType else {{ kind = "unspecified"; }};
              hasDefault = builtins.hasAttr "default" opt || builtins.hasAttr "defaultText" opt;
              hasExample = builtins.hasAttr "example" opt;
              defaultJson =
                if builtins.hasAttr "defaultText" opt then safeToJSON (safeEval opt.defaultText)
                else if builtins.hasAttr "default" opt then safeToJSON opt.default
                else null;
              exampleJson = if hasExample then safeToJSON opt.example else null;
              entry = {{
                inherit path;
                description = opt.description or "";
                type = typeSchema;
                required = !hasDefault;
              }}
              // lib.optionalAttrs (defaultJson != null) {{ default = defaultJson; }}
              // lib.optionalAttrs (exampleJson != null) {{ example = exampleJson; }};
            in if skip then [] else [ entry ];

          optionsToSchemaList = depth: stripFn: options:
            if depth > 8 then []
            else lib.concatMap (opt: optionToSchema depth stripFn opt) (lib.collect lib.isOption options);

          # --- prefix stripping ---
          mkStripPrefix = prefix:
            let plen = builtins.stringLength prefix; in
            path: let slen = builtins.stringLength path; in
              if lib.hasPrefix prefix path && slen > plen
              then builtins.substring plen (slen - plen) path
              else path;

          baseStripFn = mkStripPrefix "<name>.";

          # --- base schema ---
          baseAllOptions = optionsToSchemaList 0 baseStripFn serviceSubOpts;
          baseOptions = builtins.filter (opt: !(isModuleInternal opt.path)) baseAllOptions;

          # --- per-service schemas ---
          discoveredServiceNames = builtins.filter (name:
            let v = evalServicesOptions.${{name}} or null;
            in v != null && builtins.isAttrs v && !(lib.isOption v) && lib.isOption (v.enable or null)
          ) (builtins.attrNames evalServicesOptions);

          mkServiceSchema = name:
            let
              serviceOpts = evalServicesOptions.${{name}};
              stripFn = mkStripPrefix "services.${{name}}.";
              rawOptions = optionsToSchemaList 0 stripFn serviceOpts;
              options = builtins.filter (opt: !(isModuleInternal opt.path)) rawOptions;
              descOpt = let r = builtins.tryEval (
                if builtins.hasAttr "description" serviceOpts && lib.isOption serviceOpts.description
                then serviceOpts.description.default or ""
                else ""
              ); in if r.success then r.value else "";
              enableDesc = let r = builtins.tryEval (serviceOpts.enable.description or "");
                in if r.success then r.value else "";
              description = if descOpt != "" then descOpt else enableDesc;
            in {{ inherit description options; }};

          discoveredServices = builtins.listToAttrs (
            builtins.map (name: {{ inherit name; value = mkServiceSchema name; }}) discoveredServiceNames
          );

        in builtins.toJSON {{
          base.options = baseOptions;
          services = discoveredServices;
        }}
        "#
    );

    let output = NixCommand::new(&["eval"])
        .arg("--raw")
        .arg("--impure")
        .arg("--expr")
        .arg(&expr)
        .output()?;

    // --raw outputs the string value without nix quoting, so stdout is
    // the JSON directly.
    let schema: ServiceOptionsSchema = serde_json::from_slice(&output.stdout)?;
    Ok(schema)
}

/// Load the cached schema, or generate and cache it.
pub fn load_or_generate(flake: &str) -> color_eyre::Result<ServiceOptionsSchema> {
    if let Some(schema) = read_cache()? {
        return Ok(schema);
    }
    let spinner = ekapkgs_ui::progress::spinner("Generating service options schema...");
    let schema = generate(flake)?;
    spinner.finish_and_clear();
    write_cache(&schema)?;
    Ok(schema)
}

/// Get the options for a specific service, falling back to the base schema.
pub fn options_for_service<'a>(
    schema: &'a ServiceOptionsSchema,
    service_name: &str,
) -> &'a [OptionDef] {
    schema
        .services
        .get(service_name)
        .map(|s| s.options.as_slice())
        .unwrap_or(&schema.base.options)
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

impl OptionType {
    /// Short human-readable type description for display.
    pub fn display_name(&self) -> String {
        match self {
            Self::Bool => "bool".into(),
            Self::Str => "string".into(),
            Self::Int { positive, unsigned } => {
                if *positive {
                    "positive integer".into()
                } else if *unsigned {
                    "unsigned integer".into()
                } else {
                    "integer".into()
                }
            },
            Self::Port => "port (0-65535)".into(),
            Self::Path => "path".into(),
            Self::Lines => "multi-line string".into(),
            Self::Float => "float".into(),
            Self::Number => "number".into(),
            Self::Package => "package".into(),
            Self::Enum { values } => {
                let vals: Vec<String> = values
                    .iter()
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                format!("one of: {}", vals.join(", "))
            },
            Self::ListOf { element, .. } => format!("list of {}", element.display_name()),
            Self::AttrsOf { element } => format!("attrs of {}", element.display_name()),
            Self::NullOr { inner } => format!("null or {}", inner.display_name()),
            Self::Either { variants } => {
                let names: Vec<String> = variants.iter().map(Self::display_name).collect();
                names.join(" or ")
            },
            Self::Submodule { options } => format!("submodule ({} options)", options.len()),
            Self::Anything => "anything".into(),
            Self::Unspecified { description } => {
                description.clone().unwrap_or_else(|| "unspecified".into())
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_minimal_schema() {
        let json = r#"{
            "base": { "options": [] },
            "services": {}
        }"#;
        let schema: ServiceOptionsSchema = serde_json::from_str(json).unwrap();
        assert!(schema.base.options.is_empty());
        assert!(schema.services.is_empty());
    }

    #[test]
    fn deserialize_option_types() {
        let json = r#"{
            "base": {
                "options": [
                    {
                        "path": "enable",
                        "description": "Enable the service",
                        "type": { "kind": "bool" },
                        "required": false,
                        "default": "false"
                    },
                    {
                        "path": "port",
                        "description": "Listen port",
                        "type": { "kind": "port" },
                        "required": true
                    },
                    {
                        "path": "logLevel",
                        "description": "Log level",
                        "type": { "kind": "enum", "values": ["info", "debug", "warn"] },
                        "required": false,
                        "default": "\"info\""
                    },
                    {
                        "path": "settings",
                        "description": "Config",
                        "type": {
                            "kind": "submodule",
                            "options": [
                                {
                                    "path": "maxConns",
                                    "description": "Max connections",
                                    "type": { "kind": "int", "positive": true },
                                    "required": false,
                                    "default": "100"
                                }
                            ]
                        },
                        "required": false
                    },
                    {
                        "path": "listen",
                        "description": "Addresses",
                        "type": {
                            "kind": "listOf",
                            "element": { "kind": "str" }
                        },
                        "required": false,
                        "default": "[]"
                    },
                    {
                        "path": "command",
                        "description": "Command",
                        "type": {
                            "kind": "either",
                            "variants": [
                                { "kind": "path" },
                                { "kind": "str" }
                            ]
                        },
                        "required": true
                    }
                ]
            },
            "services": {
                "openssh": {
                    "description": "OpenSSH Daemon",
                    "options": [
                        {
                            "path": "enable",
                            "description": "Enable openssh",
                            "type": { "kind": "bool" },
                            "required": false,
                            "default": "false"
                        },
                        {
                            "path": "settings.permitRootLogin",
                            "description": "Root login policy",
                            "type": {
                                "kind": "enum",
                                "values": ["yes", "no", "prohibit-password"]
                            },
                            "required": false,
                            "default": "\"prohibit-password\""
                        }
                    ]
                }
            }
        }"#;

        let schema: ServiceOptionsSchema = serde_json::from_str(json).unwrap();
        assert_eq!(schema.base.options.len(), 6);
        assert_eq!(schema.services.len(), 1);
        assert!(schema.services.contains_key("openssh"));
        assert_eq!(schema.services["openssh"].options.len(), 2);

        // Verify type matching
        assert!(matches!(
            schema.base.options[0].option_type,
            OptionType::Bool
        ));
        assert!(matches!(
            schema.base.options[1].option_type,
            OptionType::Port
        ));
        assert!(matches!(
            schema.base.options[2].option_type,
            OptionType::Enum { .. }
        ));
        assert!(matches!(
            schema.base.options[3].option_type,
            OptionType::Submodule { .. }
        ));
        assert!(matches!(
            schema.base.options[4].option_type,
            OptionType::ListOf { .. }
        ));
        assert!(matches!(
            schema.base.options[5].option_type,
            OptionType::Either { .. }
        ));
    }

    #[test]
    fn options_for_service_fallback() {
        let schema = ServiceOptionsSchema {
            base: BaseSchema {
                options: vec![OptionDef {
                    path: "enable".into(),
                    description: "Enable".into(),
                    option_type: OptionType::Bool,
                    required: false,
                    default: Some("false".into()),
                    example: None,
                }],
            },
            services: std::collections::HashMap::new(),
        };

        // Unknown service falls back to base
        let opts = options_for_service(&schema, "unknown-service");
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].path, "enable");
    }

    #[test]
    fn display_names() {
        assert_eq!(OptionType::Bool.display_name(), "bool");
        assert_eq!(OptionType::Port.display_name(), "port (0-65535)");
        assert_eq!(
            OptionType::Enum {
                values: vec!["a".into(), "b".into()]
            }
            .display_name(),
            "one of: a, b"
        );
        assert_eq!(
            OptionType::ListOf {
                element: Box::new(OptionType::Str),
                non_empty: false,
            }
            .display_name(),
            "list of string"
        );
        assert_eq!(
            OptionType::NullOr {
                inner: Box::new(OptionType::Path)
            }
            .display_name(),
            "null or path"
        );
    }

    #[test]
    fn roundtrip_serialization() {
        let schema = ServiceOptionsSchema {
            base: BaseSchema {
                options: vec![
                    OptionDef {
                        path: "enable".into(),
                        description: "Enable".into(),
                        option_type: OptionType::Bool,
                        required: false,
                        default: Some("false".into()),
                        example: None,
                    },
                    OptionDef {
                        path: "port".into(),
                        description: "Port".into(),
                        option_type: OptionType::Port,
                        required: true,
                        default: None,
                        example: Some("8080".into()),
                    },
                ],
            },
            services: HashMap::new(),
        };

        let json = serde_json::to_string(&schema).unwrap();
        let parsed: ServiceOptionsSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.base.options.len(), 2);
        assert_eq!(parsed.base.options[0].path, "enable");
        assert_eq!(parsed.base.options[1].path, "port");
    }
}
