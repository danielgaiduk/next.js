use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use indexmap::{indexmap, IndexMap};
use turbo_tasks::{Value, Vc};
use turbopack_binding::{
    turbo::tasks_fs::{glob::Glob, FileSystem, FileSystemPath},
    turbopack::{
        core::{
            resolve::{
                options::{ConditionValue, ImportMap, ImportMapping, ResolveOptions, ResolvedMap},
                parse::Request,
                pattern::Pattern,
                resolve, AliasPattern, ResolveAliasMap, SubpathValue,
            },
            source::Source,
        },
        node::execution_context::ExecutionContext,
        turbopack::{resolve_options, resolve_options_context::ResolveOptionsContext},
    },
};

use crate::{
    embed_js::{next_js_fs, VIRTUAL_PACKAGE_NAME},
    mode::NextMode,
    next_client::context::ClientContextType,
    next_config::NextConfig,
    next_font::{
        google::{NextFontGoogleCssModuleReplacer, NextFontGoogleReplacer},
        local::{NextFontLocalCssModuleReplacer, NextFontLocalReplacer},
    },
    next_server::context::ServerContextType,
    util::NextRuntime,
};

// Make sure to not add any external requests here.
/// Computes the Next-specific client import map.
#[turbo_tasks::function]
pub async fn get_next_client_import_map(
    project_path: Vc<FileSystemPath>,
    ty: Value<ClientContextType>,
    mode: NextMode,
    next_config: Vc<NextConfig>,
    execution_context: Vc<ExecutionContext>,
) -> Result<Vc<ImportMap>> {
    let mut import_map = ImportMap::empty();

    insert_next_shared_aliases(
        &mut import_map,
        project_path,
        execution_context,
        next_config,
        mode,
    )
    .await?;

    insert_optimized_module_aliases(&mut import_map, project_path).await?;

    insert_alias_option(
        &mut import_map,
        project_path,
        next_config.resolve_alias_options(),
        ["browser"],
    )
    .await?;

    match ty.into_value() {
        ClientContextType::Pages { pages_dir } => {
            insert_alias_to_alternatives(
                &mut import_map,
                format!("{VIRTUAL_PACKAGE_NAME}/pages/_app"),
                vec![
                    request_to_import_mapping(pages_dir, "./_app"),
                    request_to_import_mapping(pages_dir, "next/app"),
                ],
            );
            insert_alias_to_alternatives(
                &mut import_map,
                format!("{VIRTUAL_PACKAGE_NAME}/pages/_document"),
                vec![
                    request_to_import_mapping(pages_dir, "./_document"),
                    request_to_import_mapping(pages_dir, "next/document"),
                ],
            );
            insert_alias_to_alternatives(
                &mut import_map,
                format!("{VIRTUAL_PACKAGE_NAME}/pages/_error"),
                vec![
                    request_to_import_mapping(pages_dir, "./_error"),
                    request_to_import_mapping(pages_dir, "next/error"),
                ],
            );
        }
        ClientContextType::App { app_dir } => {
            let react_flavor = if *next_config.enable_server_actions().await? {
                "-experimental"
            } else {
                ""
            };

            import_map.insert_exact_alias(
                "react",
                request_to_import_mapping(
                    app_dir,
                    &format!("next/dist/compiled/react{react_flavor}"),
                ),
            );
            import_map.insert_wildcard_alias(
                "react/",
                request_to_import_mapping(
                    app_dir,
                    &format!("next/dist/compiled/react{react_flavor}/*"),
                ),
            );
            import_map.insert_exact_alias(
                "react-dom",
                request_to_import_mapping(
                    app_dir,
                    &format!("next/dist/compiled/react-dom{react_flavor}"),
                ),
            );
            import_map.insert_wildcard_alias(
                "react-dom/",
                request_to_import_mapping(
                    app_dir,
                    &format!("next/dist/compiled/react-dom{react_flavor}/*"),
                ),
            );
            import_map.insert_wildcard_alias(
                "react-server-dom-webpack/",
                request_to_import_mapping(app_dir, "react-server-dom-turbopack/*"),
            );
            import_map.insert_wildcard_alias(
                "react-server-dom-turbopack/",
                request_to_import_mapping(
                    app_dir,
                    &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/*"),
                ),
            );
            import_map.insert_exact_alias(
                "next/head",
                request_to_import_mapping(project_path, "next/dist/client/components/noop-head"),
            );
            import_map.insert_exact_alias(
                "next/dynamic",
                request_to_import_mapping(project_path, "next/dist/shared/lib/app-dynamic"),
            );
        }
        ClientContextType::Fallback => {}
        ClientContextType::Other => {}
    }

    // see https://github.com/vercel/next.js/blob/8013ef7372fc545d49dbd060461224ceb563b454/packages/next/src/build/webpack-config.ts#L1449-L1531
    insert_exact_alias_map(
        &mut import_map,
        project_path,
        indexmap! {
            "server-only" => "next/dist/compiled/server-only/index".to_string(),
            "client-only" => "next/dist/compiled/client-only/index".to_string(),
            "next/dist/compiled/server-only" => "next/dist/compiled/server-only/index".to_string(),
            "next/dist/compiled/client-only" => "next/dist/compiled/client-only/index".to_string(),
        },
    );

    match ty.into_value() {
        ClientContextType::Pages { .. }
        | ClientContextType::App { .. }
        | ClientContextType::Fallback => {
            for (original, alias) in NEXT_ALIASES {
                import_map.insert_exact_alias(
                    format!("node:{original}"),
                    request_to_import_mapping(project_path, alias),
                );
            }
        }
        ClientContextType::Other => {}
    }

    insert_turbopack_dev_alias(&mut import_map);

    Ok(import_map.cell())
}

/// Computes the Next-specific client import map.
#[turbo_tasks::function]
pub fn get_next_build_import_map() -> Vc<ImportMap> {
    let mut import_map = ImportMap::empty();

    insert_package_alias(
        &mut import_map,
        &format!("{VIRTUAL_PACKAGE_NAME}/"),
        next_js_fs().root(),
    );

    let external = ImportMapping::External(None).cell();

    import_map.insert_exact_alias("next", external);
    import_map.insert_wildcard_alias("next/", external);
    import_map.insert_exact_alias("styled-jsx", external);
    import_map.insert_wildcard_alias("styled-jsx/", external);

    import_map.cell()
}

/// Computes the Next-specific client fallback import map, which provides
/// polyfills to Node.js externals.
#[turbo_tasks::function]
pub fn get_next_client_fallback_import_map(ty: Value<ClientContextType>) -> Vc<ImportMap> {
    let mut import_map = ImportMap::empty();

    match ty.into_value() {
        ClientContextType::Pages {
            pages_dir: context_dir,
        }
        | ClientContextType::App {
            app_dir: context_dir,
        } => {
            for (original, alias) in NEXT_ALIASES {
                import_map
                    .insert_exact_alias(original, request_to_import_mapping(context_dir, alias));
            }
        }
        ClientContextType::Fallback => {}
        ClientContextType::Other => {}
    }

    insert_turbopack_dev_alias(&mut import_map);

    import_map.cell()
}

/// Computes the Next-specific server-side import map.
#[turbo_tasks::function]
pub async fn get_next_server_import_map(
    project_path: Vc<FileSystemPath>,
    ty: Value<ServerContextType>,
    mode: NextMode,
    next_config: Vc<NextConfig>,
    execution_context: Vc<ExecutionContext>,
) -> Result<Vc<ImportMap>> {
    let mut import_map = ImportMap::empty();

    insert_next_shared_aliases(
        &mut import_map,
        project_path,
        execution_context,
        next_config,
        mode,
    )
    .await?;

    insert_alias_option(
        &mut import_map,
        project_path,
        next_config.resolve_alias_options(),
        [],
    )
    .await?;

    let ty = ty.into_value();

    let external: Vc<ImportMapping> = ImportMapping::External(None).cell();

    import_map.insert_exact_alias("next/dist/server/require-hook", external);
    match ty {
        ServerContextType::Pages { .. } | ServerContextType::PagesData { .. } => {
            import_map.insert_exact_alias("react", external);
            import_map.insert_wildcard_alias("react/", external);
            import_map.insert_exact_alias("react-dom", external);
            import_map.insert_wildcard_alias("react-dom/", external);
            import_map.insert_exact_alias("styled-jsx", external);
            import_map.insert_wildcard_alias("styled-jsx/", external);
            // TODO: we should not bundle next/dist/build/utils in the pages renderer at all
            import_map.insert_wildcard_alias("next/dist/build/utils", external);
        }
        ServerContextType::AppSSR { .. }
        | ServerContextType::AppRSC { .. }
        | ServerContextType::AppRoute { .. } => {
            let react_flavor = if *next_config.enable_server_actions().await? {
                "-experimental"
            } else {
                ""
            };

            import_map.insert_exact_alias(
                "private-next-rsc-action-proxy",
                request_to_import_mapping(
                    project_path,
                    "next/dist/build/webpack/loaders/next-flight-loader/action-proxy",
                ),
            );
            import_map.insert_exact_alias(
                "private-next-rsc-action-client-wrapper",
                request_to_import_mapping(
                    project_path,
                    "next/dist/build/webpack/loaders/next-flight-loader/action-client-wrapper",
                ),
            );
            import_map.insert_exact_alias(
                "private-next-rsc-action-validate",
                request_to_import_mapping(
                    project_path,
                    "next/dist/build/webpack/loaders/next-flight-loader/action-validate",
                ),
            );
            import_map.insert_exact_alias(
                "next/head",
                request_to_import_mapping(project_path, "next/dist/client/components/noop-head"),
            );
            import_map.insert_exact_alias(
                "next/dynamic",
                request_to_import_mapping(project_path, "next/dist/shared/lib/app-dynamic"),
            );

            let mapping = request_to_import_mapping(
                project_path,
                &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/client"),
            );
            import_map.insert_exact_alias("react-server-dom-webpack/client", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/client", mapping);

            let mapping = request_to_import_mapping(
                project_path,
                &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/client.edge"),
            );
            import_map.insert_exact_alias("react-server-dom-webpack/client.edge", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/client.edge", mapping);

            let mapping = request_to_import_mapping(
                project_path,
                &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/server.edge"),
            );
            import_map.insert_exact_alias("react-server-dom-webpack/server.edge", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/server.edge", mapping);

            let mapping = request_to_import_mapping(
                project_path,
                &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/server.node"),
            );
            import_map.insert_exact_alias("react-server-dom-webpack/server.node", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/server.node", mapping);
        }
        ServerContextType::Middleware => {}
    }

    insert_next_server_special_aliases(
        &mut import_map,
        project_path,
        ty,
        mode,
        NextRuntime::NodeJs,
        next_config,
    )
    .await?;

    Ok(import_map.cell())
}

/// Computes the Next-specific edge-side import map.
#[turbo_tasks::function]
pub async fn get_next_edge_import_map(
    project_path: Vc<FileSystemPath>,
    ty: Value<ServerContextType>,
    mode: NextMode,
    next_config: Vc<NextConfig>,
    execution_context: Vc<ExecutionContext>,
) -> Result<Vc<ImportMap>> {
    let mut import_map = ImportMap::empty();

    // https://github.com/vercel/next.js/blob/786ef25e529e1fb2dda398aebd02ccbc8d0fb673/packages/next/src/build/webpack-config.ts#L815-L861

    // Alias next/dist imports to next/dist/esm assets
    insert_wildcard_alias_map(
        &mut import_map,
        project_path,
        indexmap! {
            "next/dist/build/" => "next/dist/esm/build/*".to_string(),
            "next/dist/client/" => "next/dist/esm/client/*".to_string(),
            "next/dist/shared/" => "next/dist/esm/shared/*".to_string(),
            "next/dist/pages/" => "next/dist/esm/pages/*".to_string(),
            "next/dist/lib/" => "next/dist/esm/lib/*".to_string(),
            "next/dist/server/" => "next/dist/esm/server/*".to_string(),
        },
    );

    // Alias the usage of next public APIs
    insert_exact_alias_map(
        &mut import_map,
        project_path,
        indexmap! {
            "next/app" => "next/dist/esm/pages/_app".to_string(),
            "next/document" => "next/dist/esm/pages/_document".to_string(),
            "next/dynamic" => "next/dist/esm/shared/lib/dynamic".to_string(),
            "next/head" => "next/dist/esm/shared/lib/head".to_string(),
            "next/headers" => "next/dist/esm/client/components/headers".to_string(),
            "next/image" => "next/dist/esm/shared/lib/image-external".to_string(),
            "next/link" => "next/dist/esm/client/link".to_string(),
            "next/navigation" => "next/dist/esm/client/components/navigation".to_string(),
            "next/router" => "next/dist/esm/client/router".to_string(),
            "next/script" => "next/dist/esm/client/script".to_string(),
            "next/server" => "next/dist/esm/server/web/exports/index".to_string(),

            "next/dist/client/components/headers" => "next/dist/esm/client/components/headers".to_string(),
            "next/dist/client/components/navigation" => "next/dist/esm/client/components/navigation".to_string(),
            "next/dist/client/link" => "next/dist/esm/client/link".to_string(),
            "next/dist/client/router" => "next/dist/esm/client/router".to_string(),
            "next/dist/client/script" => "next/dist/esm/client/script".to_string(),
            "next/dist/pages/_app" => "next/dist/esm/pages/_app".to_string(),
            "next/dist/pages/_document" => "next/dist/esm/pages/_document".to_string(),
            "next/dist/shared/lib/dynamic" => "next/dist/esm/shared/lib/dynamic".to_string(),
            "next/dist/shared/lib/head" => "next/dist/esm/shared/lib/head".to_string(),
            "next/dist/shared/lib/image-external" => "next/dist/esm/shared/lib/image-external".to_string(),
        },
    );

    insert_next_shared_aliases(
        &mut import_map,
        project_path,
        execution_context,
        next_config,
        mode,
    )
    .await?;

    insert_optimized_module_aliases(&mut import_map, project_path).await?;

    insert_alias_option(
        &mut import_map,
        project_path,
        next_config.resolve_alias_options(),
        [],
    )
    .await?;

    let ty = ty.into_value();
    match ty {
        ServerContextType::Pages { .. } | ServerContextType::PagesData { .. } => {}
        ServerContextType::AppSSR { .. }
        | ServerContextType::AppRSC { .. }
        | ServerContextType::AppRoute { .. } => {
            let react_flavor = if *next_config.enable_server_actions().await? {
                "-experimental"
            } else {
                ""
            };

            import_map.insert_exact_alias(
                "next/head",
                request_to_import_mapping(project_path, "next/dist/client/components/noop-head"),
            );
            import_map.insert_exact_alias(
                "next/dynamic",
                request_to_import_mapping(project_path, "next/dist/shared/lib/app-dynamic"),
            );

            let mapping = request_to_import_mapping(
                project_path,
                &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/client"),
            );
            import_map.insert_exact_alias("react-server-dom-webpack/client", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/client", mapping);

            let mapping = request_to_import_mapping(
                project_path,
                &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/client.edge"),
            );
            import_map.insert_exact_alias("react-server-dom-webpack/client.edge", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/client.edge", mapping);

            let mapping = request_to_import_mapping(
                project_path,
                &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/server.edge"),
            );
            import_map.insert_exact_alias("react-server-dom-webpack/server.edge", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/server.edge", mapping);

            let mapping = request_to_import_mapping(
                project_path,
                &format!("next/dist/compiled/react-server-dom-turbopack{react_flavor}/server.node"),
            );
            import_map.insert_exact_alias("react-server-dom-webpack/server.node", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/server.node", mapping);
        }
        ServerContextType::Middleware => {}
    }

    insert_next_server_special_aliases(
        &mut import_map,
        project_path,
        ty,
        mode,
        NextRuntime::Edge,
        next_config,
    )
    .await?;

    Ok(import_map.cell())
}

pub fn get_next_client_resolved_map(
    context: Vc<FileSystemPath>,
    root: Vc<FileSystemPath>,
    mode: NextMode,
) -> Vc<ResolvedMap> {
    let glob_mappings = if mode == NextMode::Development {
        vec![]
    } else {
        vec![
            // Temporary hack to replace the hot reloader until this is passable by props in
            // next.js
            (
                context.root(),
                Glob::new(
                    "**/next/dist/client/components/react-dev-overlay/hot-reloader-client.js"
                        .to_string(),
                ),
                ImportMapping::PrimaryAlternative(
                    "@vercel/turbopack-next/dev/hot-reloader.tsx".to_string(),
                    Some(root),
                )
                .into(),
            ),
        ]
    };
    ResolvedMap {
        by_glob: glob_mappings,
    }
    .cell()
}

static NEXT_ALIASES: [(&str, &str); 23] = [
    ("assert", "next/dist/compiled/assert"),
    ("buffer", "next/dist/compiled/buffer"),
    ("constants", "next/dist/compiled/constants-browserify"),
    ("crypto", "next/dist/compiled/crypto-browserify"),
    ("domain", "next/dist/compiled/domain-browser"),
    ("http", "next/dist/compiled/stream-http"),
    ("https", "next/dist/compiled/https-browserify"),
    ("os", "next/dist/compiled/os-browserify"),
    ("path", "next/dist/compiled/path-browserify"),
    ("punycode", "next/dist/compiled/punycode"),
    ("process", "next/dist/build/polyfills/process"),
    ("querystring", "next/dist/compiled/querystring-es3"),
    ("stream", "next/dist/compiled/stream-browserify"),
    ("string_decoder", "next/dist/compiled/string_decoder"),
    ("sys", "next/dist/compiled/util"),
    ("timers", "next/dist/compiled/timers-browserify"),
    ("tty", "next/dist/compiled/tty-browserify"),
    ("url", "next/dist/compiled/native-url"),
    ("util", "next/dist/compiled/util"),
    ("vm", "next/dist/compiled/vm-browserify"),
    ("zlib", "next/dist/compiled/browserify-zlib"),
    ("events", "next/dist/compiled/events"),
    ("setImmediate", "next/dist/compiled/setimmediate"),
];

async fn insert_next_server_special_aliases(
    import_map: &mut ImportMap,
    project_path: Vc<FileSystemPath>,
    ty: ServerContextType,
    mode: NextMode,
    runtime: NextRuntime,
    next_config: Vc<NextConfig>,
) -> Result<()> {
    let external_if_node = move |context_dir: Vc<FileSystemPath>, request: &str| match runtime {
        NextRuntime::Edge => request_to_import_mapping(context_dir, request),
        NextRuntime::NodeJs => external_request_to_import_mapping(request),
    };
    match (mode, ty) {
        (_, ServerContextType::Pages { pages_dir }) => {
            import_map.insert_exact_alias(
                "@opentelemetry/api",
                // TODO(WEB-625) this actually need to prefer the local version of
                // @opentelemetry/api
                external_if_node(pages_dir, "next/dist/compiled/@opentelemetry/api"),
            );
            insert_alias_to_alternatives(
                import_map,
                format!("{VIRTUAL_PACKAGE_NAME}/pages/_app"),
                vec![
                    request_to_import_mapping(pages_dir, "./_app"),
                    external_if_node(pages_dir, "next/app"),
                ],
            );
            insert_alias_to_alternatives(
                import_map,
                format!("{VIRTUAL_PACKAGE_NAME}/pages/_document"),
                vec![
                    request_to_import_mapping(pages_dir, "./_document"),
                    external_if_node(pages_dir, "next/document"),
                ],
            );
            insert_alias_to_alternatives(
                import_map,
                format!("{VIRTUAL_PACKAGE_NAME}/pages/_error"),
                vec![
                    request_to_import_mapping(pages_dir, "./_error"),
                    external_if_node(pages_dir, "next/error"),
                ],
            );
        }
        (_, ServerContextType::PagesData { .. }) => {}
        // the logic closely follows the one in createRSCAliases in webpack-config.ts
        (NextMode::Build | NextMode::Development, ServerContextType::AppSSR { app_dir }) => {
            import_map.insert_exact_alias(
                "@opentelemetry/api",
                // TODO(WEB-625) this actually need to prefer the local version of
                // @opentelemetry/api
                request_to_import_mapping(app_dir, "next/dist/compiled/@opentelemetry/api"),
            );
            import_map.insert_exact_alias(
                "styled-jsx",
                request_to_import_mapping(get_next_package(app_dir), "styled-jsx"),
            );
            import_map.insert_wildcard_alias(
                "styled-jsx/",
                request_to_import_mapping(get_next_package(app_dir), "styled-jsx/*"),
            );

            let server_actions = *next_config.enable_server_actions().await?;
            import_map.insert_exact_alias(
                "react/jsx-runtime",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => {
                            "next/dist/compiled/react-experimental/jsx-runtime"
                        }
                        (NextRuntime::Edge, false) => "next/dist/compiled/react/jsx-runtime",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/ssr/\
                             react-jsx-runtime"
                        }
                    },
                ),
            );
            import_map.insert_exact_alias(
                "react/jsx-dev-runtime",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => {
                            "next/dist/compiled/react-experimental/jsx-dev-runtime"
                        }
                        (NextRuntime::Edge, false) => "next/dist/compiled/react/jsx-dev-runtime",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/ssr/\
                             react-jsx-dev-runtime"
                        }
                    },
                ),
            );
            import_map.insert_exact_alias(
                "react",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => "next/dist/compiled/react-experimental",
                        (NextRuntime::Edge, false) => "next/dist/compiled/react",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/ssr/react"
                        }
                    },
                ),
            );
            import_map.insert_exact_alias(
                "react-dom",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => "next/dist/compiled/react-dom-experimental",
                        (NextRuntime::Edge, false) => "next/dist/compiled/react-dom",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/ssr/react-dom"
                        }
                    },
                ),
            );

            let mapping = request_to_import_mapping(
                app_dir,
                match (runtime, server_actions) {
                    (NextRuntime::Edge, true) => {
                        "next/dist/compiled/react-server-dom-turbopack-experimental/client.edge"
                    }
                    (NextRuntime::Edge, false) => {
                        "next/dist/compiled/react-server-dom-turbopack/client.edge"
                    }
                    // When we access the runtime we still use the webpack name. The runtime
                    // itself will substitute in the turbopack variant
                    (NextRuntime::NodeJs, _) => {
                        "next/dist/server/future/route-modules/app-page/vendored/ssr/\
                         react-server-dom-turbopack-client-edge"
                    }
                },
            );
            import_map.insert_exact_alias("react-server-dom-webpack/client.edge", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/client.edge", mapping);

            // import_map.insert_exact_alias("react-server-dom-turbopack/client", mapping);
            // not essential but we're providing this alias for people who might use it.
            // A note here is that this will point toward the ReactDOMServer on the SSR
            // layer TODO: add the rests
            import_map.insert_exact_alias(
                "react-dom/server",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => {
                            "next/dist/compiled/react-dom-experimental/server.edge"
                        }
                        (NextRuntime::Edge, false) => "next/dist/compiled/react-dom/server.edge",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/ssr/\
                             react-dom-server-edge"
                        }
                    },
                ),
            );

            import_map.insert_exact_alias(
                "react-dom/server.edge",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => {
                            "next/dist/compiled/react-dom-experimental/server.edge"
                        }
                        (NextRuntime::Edge, false) => "next/dist/compiled/react-dom/server.edge",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/ssr/\
                             react-dom-server-edge"
                        }
                    },
                ),
            );
        }
        (
            NextMode::Build | NextMode::Development,
            ServerContextType::AppRSC { app_dir, .. } | ServerContextType::AppRoute { app_dir },
        ) => {
            import_map.insert_exact_alias(
                "@opentelemetry/api",
                // TODO(WEB-625) this actually need to prefer the local version of
                // @opentelemetry/api
                request_to_import_mapping(app_dir, "next/dist/compiled/@opentelemetry/api"),
            );
            import_map.insert_exact_alias(
                "styled-jsx",
                request_to_import_mapping(get_next_package(app_dir), "styled-jsx"),
            );
            import_map.insert_wildcard_alias(
                "styled-jsx/",
                request_to_import_mapping(get_next_package(app_dir), "styled-jsx/*"),
            );

            let server_actions = *next_config.enable_server_actions().await?;
            import_map.insert_exact_alias(
                "react/jsx-runtime",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => {
                            "next/dist/compiled/react-experimental/jsx-runtime"
                        }
                        (NextRuntime::Edge, false) => "next/dist/compiled/react/jsx-runtime",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/rsc/\
                             react-jsx-runtime"
                        }
                    },
                ),
            );
            import_map.insert_exact_alias(
                "react/jsx-dev-runtime",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => {
                            "next/dist/compiled/react-experimental/jsx-dev-runtime"
                        }
                        (NextRuntime::Edge, false) => "next/dist/compiled/react/jsx-dev-runtime",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/rsc/\
                             react-jsx-dev-runtime"
                        }
                    },
                ),
            );
            import_map.insert_exact_alias(
                "react",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => "next/dist/compiled/react-experimental",
                        (NextRuntime::Edge, false) => "next/dist/compiled/react",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/rsc/react"
                        }
                    },
                ),
            );
            import_map.insert_exact_alias(
                "react-dom",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => "next/dist/compiled/react-dom-experimental",
                        (NextRuntime::Edge, false) => "next/dist/compiled/react-dom",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/rsc/react-dom"
                        }
                    },
                ),
            );

            let mapping = request_to_import_mapping(
                app_dir,
                match (runtime, server_actions) {
                    (NextRuntime::Edge, true) => {
                        "next/dist/compiled/react-server-dom-turbopack-experimental/server.edge"
                    }
                    (NextRuntime::Edge, false) => {
                        "next/dist/compiled/react-server-dom-turbopack/server.edge"
                    }
                    // When we access the runtime we still use the webpack name. The runtime
                    // itself will substitute in the turbopack variant
                    (NextRuntime::NodeJs, _) => {
                        "next/dist/server/future/route-modules/app-page/vendored/rsc/\
                         react-server-dom-turbopack-server-edge"
                    }
                },
            );
            import_map.insert_exact_alias("react-server-dom-webpack/server.edge", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/server.edge", mapping);

            let mapping = request_to_import_mapping(
                app_dir,
                match (runtime, server_actions) {
                    (NextRuntime::Edge, true) => {
                        "next/dist/compiled/react-server-dom-turbopack-experimental/server.node"
                    }
                    (NextRuntime::Edge, false) => {
                        "next/dist/compiled/react-server-dom-turbopack/server.node"
                    }
                    // When we access the runtime we still use the webpack name. The runtime
                    // itself will substitute in the turbopack variant
                    (NextRuntime::NodeJs, _) => {
                        "next/dist/server/future/route-modules/app-page/vendored/rsc/\
                         react-server-dom-turbopack-server-node"
                    }
                },
            );
            import_map.insert_exact_alias("react-server-dom-webpack/server.node", mapping);
            import_map.insert_exact_alias("react-server-dom-turbopack/server.node", mapping);

            // not essential but we're providing this alias for people who might use it.
            // A note here is that this will point toward the ReactDOMServer on the SSR
            // layer TODO: add the rests
            import_map.insert_exact_alias(
                "react-dom/server.edge",
                request_to_import_mapping(
                    app_dir,
                    match (runtime, server_actions) {
                        (NextRuntime::Edge, true) => {
                            "next/dist/compiled/react-dom-experimental/server.edge"
                        }
                        (NextRuntime::Edge, false) => "next/dist/compiled/react-dom/server.edge",
                        (NextRuntime::NodeJs, _) => {
                            "next/dist/server/future/route-modules/app-page/vendored/ssr/\
                             react-dom-server-edge"
                        }
                    },
                ),
            );
        }
        (_, ServerContextType::Middleware) => {}
    }

    // see https://github.com/vercel/next.js/blob/8013ef7372fc545d49dbd060461224ceb563b454/packages/next/src/build/webpack-config.ts#L1449-L1531
    match ty {
        ServerContextType::Pages { .. }
        | ServerContextType::PagesData { .. }
        | ServerContextType::AppSSR { .. } => {
            insert_exact_alias_map(
                import_map,
                project_path,
                indexmap! {
                    "server-only" => "next/dist/compiled/server-only/index".to_string(),
                    "client-only" => "next/dist/compiled/client-only/index".to_string(),
                    "next/dist/compiled/server-only" => "next/dist/compiled/server-only/index".to_string(),
                    "next/dist/compiled/client-only" => "next/dist/compiled/client-only/index".to_string(),
                },
            );
        }
        // TODO: should include `ServerContextType::PagesApi` routes, but that type doesn't exist.
        ServerContextType::AppRSC { .. }
        | ServerContextType::AppRoute { .. }
        | ServerContextType::Middleware => {
            insert_exact_alias_map(
                import_map,
                project_path,
                indexmap! {
                    "server-only" => "next/dist/compiled/server-only/empty".to_string(),
                    "client-only" => "next/dist/compiled/client-only/error".to_string(),
                    "next/dist/compiled/server-only" => "next/dist/compiled/server-only/empty".to_string(),
                    "next/dist/compiled/client-only" => "next/dist/compiled/client-only/error".to_string(),
                },
            );
        }
    }

    // Potential the bundle introduced into middleware and api can be poisoned by
    // client-only but not being used, so we disabled the `client-only` erroring
    // on these layers. `server-only` is still available.
    if ty == ServerContextType::Middleware {
        insert_exact_alias_map(
            import_map,
            project_path,
            indexmap! {
                "client-only" => "next/dist/compiled/client-only/index".to_string(),
                "next/dist/compiled/client-only" => "next/dist/compiled/client-only/index".to_string(),
                "next/dist/compiled/client-only/error" => "next/dist/compiled/client-only/index".to_string(),
            },
        );
    }

    import_map.insert_exact_alias(
        "@vercel/og",
        external_if_node(
            project_path,
            "next/dist/server/web/spec-extension/image-response",
        ),
    );

    Ok(())
}

pub fn mdx_import_source_file() -> String {
    format!("{VIRTUAL_PACKAGE_NAME}/mdx-import-source")
}

// Insert aliases for Next.js stubs of fetch, object-assign, and url
// Keep in sync with getOptimizedModuleAliases in webpack-config.ts
async fn insert_optimized_module_aliases(
    import_map: &mut ImportMap,
    project_path: Vc<FileSystemPath>,
) -> Result<()> {
    insert_exact_alias_map(
        import_map,
        project_path,
        indexmap! {
            "unfetch" => "next/dist/build/polyfills/fetch/index.js".to_string(),
            "isomorphic-unfetch" => "next/dist/build/polyfills/fetch/index.js".to_string(),
            "whatwg-fetch" => "next/dist/build/polyfills/fetch/whatwg-fetch.js".to_string(),
            "object-assign" => "next/dist/build/polyfills/object-assign.js".to_string(),
            "object.assign/auto" => "next/dist/build/polyfills/object.assign/auto.js".to_string(),
            "object.assign/implementation" => "next/dist/build/polyfills/object.assign/implementation.js".to_string(),
            "object.assign/polyfill" => "next/dist/build/polyfills/object.assign/polyfill.js".to_string(),
            "object.assign/shim" => "next/dist/build/polyfills/object.assign/shim.js".to_string(),
            "url" => "next/dist/compiled/native-url".to_string(),
        },
    );
    Ok(())
}

// Make sure to not add any external requests here.
async fn insert_next_shared_aliases(
    import_map: &mut ImportMap,
    project_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
    next_config: Vc<NextConfig>,
    mode: NextMode,
) -> Result<()> {
    let package_root = next_js_fs().root();

    if *next_config.mdx_rs().await? {
        insert_alias_to_alternatives(
            import_map,
            mdx_import_source_file(),
            vec![
                request_to_import_mapping(project_path, "./mdx-components"),
                request_to_import_mapping(project_path, "./src/mdx-components"),
                request_to_import_mapping(project_path, "@mdx-js/react"),
            ],
        );
    }

    if mode != NextMode::Development {
        // we use the next.js hydration code, so we replace the error overlay with our
        // own
        import_map.insert_exact_alias(
            "next/dist/compiled/@next/react-dev-overlay/dist/client",
            request_to_import_mapping(package_root, "./overlay/client.ts"),
        );
    }

    insert_package_alias(
        import_map,
        &format!("{VIRTUAL_PACKAGE_NAME}/"),
        package_root,
    );

    import_map.insert_alias(
        // Request path from js via next-font swc transform
        AliasPattern::exact("next/font/google/target.css"),
        ImportMapping::Dynamic(Vc::upcast(NextFontGoogleReplacer::new(project_path))).into(),
    );

    import_map.insert_alias(
        // Request path from js via next-font swc transform
        AliasPattern::exact("@next/font/google/target.css"),
        ImportMapping::Dynamic(Vc::upcast(NextFontGoogleReplacer::new(project_path))).into(),
    );

    import_map.insert_alias(
        AliasPattern::exact("@vercel/turbopack-next/internal/font/google/cssmodule.module.css"),
        ImportMapping::Dynamic(Vc::upcast(NextFontGoogleCssModuleReplacer::new(
            project_path,
            execution_context,
        )))
        .into(),
    );

    import_map.insert_alias(
        // Request path from js via next-font swc transform
        AliasPattern::exact("next/font/local/target.css"),
        ImportMapping::Dynamic(Vc::upcast(NextFontLocalReplacer::new(project_path))).into(),
    );

    import_map.insert_alias(
        // Request path from js via next-font swc transform
        AliasPattern::exact("@next/font/local/target.css"),
        ImportMapping::Dynamic(Vc::upcast(NextFontLocalReplacer::new(project_path))).into(),
    );

    import_map.insert_alias(
        AliasPattern::exact("@vercel/turbopack-next/internal/font/local/cssmodule.module.css"),
        ImportMapping::Dynamic(Vc::upcast(NextFontLocalCssModuleReplacer::new(
            project_path,
        )))
        .into(),
    );

    import_map.insert_singleton_alias("@swc/helpers", get_next_package(project_path));
    import_map.insert_singleton_alias("styled-jsx", get_next_package(project_path));
    import_map.insert_singleton_alias("next", project_path);
    import_map.insert_singleton_alias("react", project_path);
    import_map.insert_singleton_alias("react-dom", project_path);

    //https://github.com/vercel/next.js/blob/f94d4f93e4802f951063cfa3351dd5a2325724b3/packages/next/src/build/webpack-config.ts#L1196
    import_map.insert_exact_alias(
        "setimmediate",
        request_to_import_mapping(project_path, "next/dist/compiled/setimmediate"),
    );

    insert_turbopack_dev_alias(import_map);
    insert_package_alias(
        import_map,
        "@vercel/turbopack-node/",
        turbopack_binding::turbopack::node::embed_js::embed_fs().root(),
    );

    Ok(())
}

#[turbo_tasks::function]
async fn package_lookup_resolve_options(
    project_path: Vc<FileSystemPath>,
) -> Result<Vc<ResolveOptions>> {
    Ok(resolve_options(
        project_path,
        ResolveOptionsContext {
            enable_node_modules: Some(project_path.root().resolve().await?),
            enable_node_native_modules: true,
            custom_conditions: vec!["development".to_string()],
            ..Default::default()
        }
        .cell(),
    ))
}

#[turbo_tasks::function]
pub async fn get_next_package(context_directory: Vc<FileSystemPath>) -> Result<Vc<FileSystemPath>> {
    let result = resolve(
        context_directory,
        Request::parse(Value::new(Pattern::Constant(
            "next/package.json".to_string(),
        ))),
        package_lookup_resolve_options(context_directory),
    );
    let source = result
        .first_source()
        .await?
        .context("Next.js package not found")?;
    Ok(source.ident().path().parent())
}

pub async fn insert_alias_option<const N: usize>(
    import_map: &mut ImportMap,
    project_path: Vc<FileSystemPath>,
    alias_options: Vc<ResolveAliasMap>,
    conditions: [&'static str; N],
) -> Result<()> {
    let conditions = BTreeMap::from(conditions.map(|c| (c.to_string(), ConditionValue::Set)));
    for (alias, value) in &alias_options.await? {
        if let Some(mapping) = export_value_to_import_mapping(value, &conditions, project_path) {
            import_map.insert_alias(alias, mapping);
        }
    }
    Ok(())
}

fn export_value_to_import_mapping(
    value: &SubpathValue,
    conditions: &BTreeMap<String, ConditionValue>,
    project_path: Vc<FileSystemPath>,
) -> Option<Vc<ImportMapping>> {
    let mut result = Vec::new();
    value.add_results(
        conditions,
        &ConditionValue::Unset,
        &mut HashMap::new(),
        &mut result,
    );
    if result.is_empty() {
        None
    } else {
        Some(if result.len() == 1 {
            ImportMapping::PrimaryAlternative(result[0].to_string(), Some(project_path)).cell()
        } else {
            ImportMapping::Alternatives(
                result
                    .iter()
                    .map(|m| {
                        ImportMapping::PrimaryAlternative(m.to_string(), Some(project_path)).cell()
                    })
                    .collect(),
            )
            .cell()
        })
    }
}

fn insert_exact_alias_map(
    import_map: &mut ImportMap,
    project_path: Vc<FileSystemPath>,
    map: IndexMap<&'static str, String>,
) {
    for (pattern, request) in map {
        import_map.insert_exact_alias(pattern, request_to_import_mapping(project_path, &request));
    }
}

fn insert_wildcard_alias_map(
    import_map: &mut ImportMap,
    project_path: Vc<FileSystemPath>,
    map: IndexMap<&'static str, String>,
) {
    for (pattern, request) in map {
        import_map
            .insert_wildcard_alias(pattern, request_to_import_mapping(project_path, &request));
    }
}

/// Inserts an alias to an alternative of import mappings into an import map.
fn insert_alias_to_alternatives<'a>(
    import_map: &mut ImportMap,
    alias: impl Into<String> + 'a,
    alternatives: Vec<Vc<ImportMapping>>,
) {
    import_map.insert_exact_alias(alias, ImportMapping::Alternatives(alternatives).into());
}

/// Inserts an alias to an import mapping into an import map.
fn insert_package_alias(
    import_map: &mut ImportMap,
    prefix: &str,
    package_root: Vc<FileSystemPath>,
) {
    import_map.insert_wildcard_alias(
        prefix,
        ImportMapping::PrimaryAlternative("./*".to_string(), Some(package_root)).cell(),
    );
}

/// Inserts an alias to @vercel/turbopack-dev into an import map.
fn insert_turbopack_dev_alias(import_map: &mut ImportMap) {
    insert_package_alias(
        import_map,
        "@vercel/turbopack-ecmascript-runtime/",
        turbopack_binding::turbopack::ecmascript_runtime::embed_fs().root(),
    );
}

/// Creates a direct import mapping to the result of resolving a request
/// in a context.
fn request_to_import_mapping(context_path: Vc<FileSystemPath>, request: &str) -> Vc<ImportMapping> {
    ImportMapping::PrimaryAlternative(request.to_string(), Some(context_path)).cell()
}

/// Creates a direct import mapping to the result of resolving an external
/// request.
fn external_request_to_import_mapping(request: &str) -> Vc<ImportMapping> {
    ImportMapping::External(Some(request.to_string())).into()
}
