# Cool Request feature roadmap

Reference docs: <https://coolrequest.dev/docs/intro>. This plugin is a clean-room
OpenIDEA implementation; it does not copy Cool Request code.

Status: **0.46.5**.

## Implemented

| Area | Feature |
| --- | --- |
| Scan | Spring `@Controller` / `@RestController`, class + method mappings, path arrays, `@RequestParam`, `@RequestBody`, `consumes`/`produces`, mapping headers; Java + common Kotlin |
| Host | Nearest module `application.properties`/`.yml`/`.yaml`, project root fallback, `application-<profile>.*`, `server.port`, `server.servlet.context-path`, `server.ssl.enabled` |
| Request | Method, server, path, path params, query, headers, JSON/text/form body, JSON formatter, ⌘/Ctrl+Enter |
| cURL | Copy as cURL and Import cURL (common `-X`, `-H`, `-d`, `--json`, `-u`, `--url`, cookies/UA/referer); no `-F` yet |
| Response | Status, time, size, headers, raw/pretty JSON, JSON Tree, XML indentation, HTML preview sandbox, image/binary detection, truncation, cached last response |
| State | Per-endpoint parameter + response cache on send; history (30); unsent edits discarded on switch |
| Environments | IntelliJ-style `http-client.env.json` / private env, `$shared`, module/root precedence; `{{var}}` resolution |
| Export | Save `.http`, Export OpenAPI 3, Save response text |
| Navigation | Hummingbird gutter action on the method line, Select Endpoint at Caret, Search API picker, plugin/view icon |
| Collections | Save/delete requests with folders, rename, checkbox selection, iteration counts (1–20), sequential batch run with per-entry status |
| Stress | Sequential count loop (1–200), ok/failed, avg/min/max; no parallel threads in the WASM sandbox |
| Env globals | `$shared`/env `params`, `headers`, `form` objects merged into the current request and removed on env switch; in-app editor stored in plugin storage |
| Gateway (static) | Java/Kotlin `route("id", ... .path(...).uri(...))` and `application.yml` `spring.cloud.gateway.routes` become environments |
| Tabs | Per-endpoint request tabs with independent draft/response; switch, close, drag-reorder; persisted and restored |
| WebSocket | Connect/send/close, 300 ms message drain, text log, binary frames shown base64 |
| Apifox | Optional import-openapi export with project ID + Bearer token; token not stored |
| Multipart | Form-data rows (Text/File), project file picker, byte upload, cURL `-F` import/export |
| Timeout / cookies | Per-request timeout 100–300000 ms; per-host Set-Cookie jar replayed as `Cookie`, clear action |
| Files | `.openidea/api-client/*.http`, `openapi.json`, `responses/*` |

## Next (plugin API work)

1. Apipost export (separate Open API endpoint/token model).
2. Tab float/split groups.
3. SSE streaming when the host exposes a streaming API.

## Runtime agent foundation (0.14.0)

- OpenIDEA host supports manifest `java-agent`, capability `java.agent`, and
  injects `-javaagent` into Java run configurations at start (frontend
  `src/features/run/jvmAgent.ts`).
- The plugin ships a Java agent that starts a loopback server, bundles a
  `SpringApplicationRunListener`, captures the `ApplicationContext` and exposes
  `/health` + `/mappings`. Verified with foodie on Spring Boot 2.3.1/JDK 11.
- `/invoke` reflection mode: direct `HandlerMethod` invocation with path/query/
  header/JSON-body argument resolution, bypassing filters and interceptors;
  verified against foodie (`HelloController.hello` returns `Hello World`).
- `/schedules` + `/triggerSchedule` and `/routes`: runtime `@Scheduled` scan/
  trigger and Gateway `RouteDefinitionLocator` listing.
- Runtime mappings are rendered as a **Runtime** group and selected with the
  application's real `serverPort`/`contextPath`.
- Release OpenIDEA app built with the host injection changes:
  `openidea/target/release/bundle/macos/OpenIDEA.app` (unsigned local build;
  updater artifacts disabled so no release key is needed).
- Host injection verified end-to-end in the debug OpenIDEA app: clicking Run for
  `Application` put `-javaagent:.../agent/openidea-spring-agent.jar` on the Java
  process and the agent wrote `context:true`. Two fixes were needed: load the
  plugin store before Run, and inject into `config.args` as well as
  `params.vmOptions` (temporary Application args are built when the config is
  created).
- Trace: ByteBuddy advice on application bean methods, ring buffer, `/trace`
  toggle and a live panel; verified on foodie (`HelloController.hello`).
- SQL: MyBatis interceptor registration (guarded against duplicate installs),
  statement/SQL/parameter/duration/error ring buffer and a live panel; verified
  on foodie (`UserAddressMapper.selectList`).
- Java Script: JDK in-memory compiler, `ScriptApi` bean bridge, explicit enable/
  timeout and a Script panel; verified on foodie (`return "hello from script"`,
  and `ScriptApi.getBean("helloController")`).
- IDEA-style icon set adapted from OpenIDEA EXPUI (Apache-2.0): rail, project/module
  folders, class, runtime plugin, refresh, search and tree chevrons.
- Design pass: single blue accent, neutral layered surfaces, consistent 6px/8px radii,
  tabular counts, subtle 140ms motion with reduced-motion support; removed
  visible em-dashes and decorative page kickers.
- API tree now mirrors the source hierarchy: project/module (API counts) →
  controller (package + Javadoc) → endpoint (Javadoc); groups collapse/expand.
  Single click opens the mapping source; double click opens the request view.
- Four-view left rail: API list, request/debug, request history and settings;
  settings persist locally (advanced tools, click navigation, parameter prompt,
  auto mock, multi-line tabs, API count).
- UI redesign: two-pane workspace, endpoint navigation, request tab strip, environment
  bar, Header/Param/Path/Body/Script tabs and a compact More menu; reference is the
  current Cool Request preview (clean-room CSS, no GPL assets copied).
- Reflection fix: static endpoint drafts carry `controller`/`methodName`; without
  them `/invoke` received empty values ("No runtime handler for .").
- Reflection off fix: a JSON `null` reflection field is filtered out, so unchecked
  requests take the normal HTTP path.
- Gateway panel: runtime route id/URI/predicates, with count in Runtime.
- MCP: local JSON-RPC `/mcp` with initialize/tools/list/tools/call; tools for
  beans, method calls, mappings, schedules, routes, SQL, Trace and scripts; a
  plugin MCP panel and external-client endpoint/token. Verified on foodie
  (`getObjectInstances` + `callMethod` -> `Hello World`).

## Needs a JVM agent or external process

These cannot be implemented by the OpenIDEA plugin sandbox alone because they
require code inside the running Spring Boot JVM:

- Dynamic scan from `RequestMappingHandlerMapping` (real runtime host/ports).
- Reflection requests, interceptor bypass, proxy/original object selection.
- Call this method (container beans, static methods, xxl-job, Dubbo).
- Method trace / bytecode instrumentation.
- SQL logs and MyBatis static execution.
- Quick test code injection.
- Java pre/post scripts and custom functions that call project classes.
- MCP `getObjectInstances` / `callMethod` (needs the agent service; port 8019).

A viable next project is a companion Java agent (clean-room) plus an attach or
`-javaagent` channel to the plugin. Reflection requests and dynamic scan should
be the first round-trip to validate attach, serialization and lifecycle. Static
MyBatis execution is different: it needs a separate JVM subprocess, not an agent.

Static resources server and tunneling also need a listening socket or a third
party; the plugin API only exposes outbound fetch today.
