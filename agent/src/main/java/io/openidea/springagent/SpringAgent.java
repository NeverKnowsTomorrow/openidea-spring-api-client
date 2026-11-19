package io.openidea.springagent;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpHandler;
import com.sun.net.httpserver.HttpServer;

import java.io.IOException;
import java.io.OutputStream;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.lang.annotation.Annotation;
import java.lang.instrument.Instrumentation;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.time.Instant;
import java.util.Collection;
import java.util.Map;
import java.util.UUID;

/**
 * Minimal OpenIDEA Spring agent. It proves that the host injected the agent and
 * exposes a local health endpoint; Spring-context discovery is layered on top.
 */
public final class SpringAgent {
    private static volatile boolean started;
    private static HttpServer server;
    private static volatile Object applicationContext;
    private static volatile String token = "";
    private static volatile String mode = "";
    private static volatile String startedAt = "";
    private static volatile Instrumentation instrumentation;
    private static volatile boolean sqlTraceInstalled;
    private static final ObjectMapper MAPPER = new ObjectMapper();

    private SpringAgent() {
    }

    public static void premain(String args, Instrumentation instrumentation) {
        start("premain", args, instrumentation);
    }

    public static void agentmain(String args, Instrumentation instrumentation) {
        start("agentmain", args, instrumentation);
    }

    private static synchronized void start(String startMode, String args, Instrumentation instrumentation) {
        if (started) {
            return;
        }
        started = true;
        mode = startMode;
        SpringAgent.instrumentation = instrumentation;
        try {
            appendAgentToClasspath(instrumentation);
            Path dir = Paths.get(System.getProperty("user.home"), ".openidea", "spring-api-client");
            Files.createDirectories(dir);
            token = UUID.randomUUID().toString();
            startedAt = Instant.now().toString();
            server = HttpServer.create(new InetSocketAddress(InetAddress.getLoopbackAddress(), 0), 0);
            server.createContext("/health", new HealthHandler());
            server.createContext("/mappings", new MappingsHandler());
            server.createContext("/invoke", new InvokeHandler());
            server.createContext("/method/invoke", new MethodInvokeHandler());
            server.createContext("/schedules", new SchedulesHandler());
            server.createContext("/triggerSchedule", new TriggerScheduleHandler());
            server.createContext("/routes", new RoutesHandler());
            server.createContext("/trace", new TraceHandler());
            server.createContext("/sql", new SqlHandler());
            server.createContext("/script", new ScriptHandler());
            server.createContext("/script/run", new ScriptRunHandler());
            server.createContext("/mcp", new McpHandler());
            server.setExecutor(command -> {
                Thread thread = new Thread(command, "openidea-spring-agent");
                thread.setDaemon(true);
                thread.start();
            });
            server.start();
            writeRuntime();
            Runtime.getRuntime().addShutdownHook(new Thread(() -> {
                if (server != null) {
                    server.stop(0);
                }
            }));
        } catch (Throwable error) {
            try {
                Path dir = Paths.get(System.getProperty("user.home"), ".openidea", "spring-api-client");
                Files.createDirectories(dir);
                Files.write(dir.resolve("agent-error.log"),
                        (error.toString() + System.lineSeparator()).getBytes(StandardCharsets.UTF_8));
            } catch (Throwable ignored) {
                // The agent must never break the application.
            }
        }
    }

    static synchronized void contextReady(Object context) {
        if (context == null) {
            return;
        }
        applicationContext = context;
        installSqlTrace(context);
        writeRuntime();
    }

    private static synchronized void writeRuntime() {
        try {
            Path dir = Paths.get(System.getProperty("user.home"), ".openidea", "spring-api-client");
            Files.createDirectories(dir);
            String runtime = "{\"port\":" + (server == null ? 0 : server.getAddress().getPort())
                    + ",\"token\":\"" + token + "\""
                    + ",\"pid\":" + pid()
                    + ",\"mode\":\"" + mode + "\""
                    + ",\"context\":" + (applicationContext != null)
                    + ",\"contextClass\":\"" + (applicationContext == null ? "" : applicationContext.getClass().getName()) + "\""
                    + ",\"serverPort\":" + serverPort()
                    + ",\"contextPath\":\"" + escape(contextPath()) + "\""
                    + ",\"debug\":" + debuggerAttached()
                    + ",\"startedAt\":\"" + startedAt + "\"}";
            Files.write(dir.resolve("runtime.json"), runtime.getBytes(StandardCharsets.UTF_8));
            Path project = Paths.get(System.getProperty("user.dir"), ".openidea");
            Files.createDirectories(project);
            Files.write(project.resolve("spring-agent.json"), runtime.getBytes(StandardCharsets.UTF_8));
        } catch (Throwable ignored) {
        }
    }

    private static boolean debuggerAttached() {
        try {
            for (String arg : java.lang.management.ManagementFactory.getRuntimeMXBean().getInputArguments()) {
                if (arg.startsWith("-agentlib:jdwp") || arg.startsWith("-Xrunjdwp")) {
                    return true;
                }
            }
        } catch (Throwable ignored) {
        }
        return false;
    }

    private static void appendAgentToClasspath(Instrumentation instrumentation) {
        try {
            java.security.CodeSource source = SpringAgent.class.getProtectionDomain().getCodeSource();
            if (source == null || source.getLocation() == null) {
                return;
            }
            java.net.URL location = source.getLocation();
            java.io.File file = new java.io.File(location.toURI());
            if (file.isFile()) {
                instrumentation.appendToSystemClassLoaderSearch(new java.util.jar.JarFile(file));
            }
        } catch (Throwable ignored) {
        }
    }

    private static String mappingsJson() {
        Object context = applicationContext;
        if (context == null) {
            return "{\"context\":false,\"mappings\":[]}";
        }
        StringBuilder out = new StringBuilder("{\"context\":true,\"mappings\":[");
        try {
            Object mapping = invoke(context, "getBean", new Class<?>[]{String.class}, new Object[]{"requestMappingHandlerMapping"});
            Object methods = invoke(mapping, "getHandlerMethods", new Class<?>[0], new Object[0]);
            if (methods instanceof Map) {
                boolean first = true;
                for (Object entryObject : ((Map<?, ?>) methods).entrySet()) {
                    Map.Entry<?, ?> entry = (Map.Entry<?, ?>) entryObject;
                    Object info = entry.getKey();
                    Object handler = entry.getValue();
                    String path = firstPattern(info);
                    String method = firstMethod(info);
                    Object beanType = invoke(handler, "getBeanType", new Class<?>[0], new Object[0]);
                    Object methodObject = invoke(handler, "getMethod", new Class<?>[0], new Object[0]);
                    String controller = beanType instanceof Class ? ((Class<?>) beanType).getSimpleName() : "";
                    String methodName = methodObject instanceof Method ? ((Method) methodObject).getName() : "";
                    if (!first) out.append(',');
                    first = false;
                    out.append("{\"method\":\"").append(escape(method))
                            .append("\",\"path\":\"").append(escape(path))
                            .append("\",\"controller\":\"").append(escape(controller))
                            .append("\",\"methodName\":\"").append(escape(methodName)).append("\"}");
                }
            }
        } catch (Throwable error) {
            return "{\"context\":true,\"error\":\"" + escape(error.toString()) + "\",\"mappings\":[]}";
        }
        return out.append("]}").toString();
    }

    private static Object invoke(Object target, String name, Class<?>[] types, Object[] args) throws Exception {
        Method method = target.getClass().getMethod(name, types);
        method.setAccessible(true);
        return method.invoke(target, args);
    }

    private static String firstPattern(Object info) {
        try {
            Object condition = invoke(info, "getPatternsCondition", new Class<?>[0], new Object[0]);
            Object patterns = invoke(condition, "getPatterns", new Class<?>[0], new Object[0]);
            if (patterns instanceof Collection && !((Collection<?>) patterns).isEmpty()) {
                return String.valueOf(((Collection<?>) patterns).iterator().next());
            }
        } catch (Throwable ignored) {
        }
        try {
            Object condition = invoke(info, "getPathPatternsCondition", new Class<?>[0], new Object[0]);
            Object patterns = invoke(condition, "getPatternValues", new Class<?>[0], new Object[0]);
            if (patterns instanceof Collection && !((Collection<?>) patterns).isEmpty()) {
                return String.valueOf(((Collection<?>) patterns).iterator().next());
            }
        } catch (Throwable ignored) {
        }
        return String.valueOf(info);
    }

    private static String firstMethod(Object info) {
        try {
            Object condition = invoke(info, "getMethodsCondition", new Class<?>[0], new Object[0]);
            Object methods = invoke(condition, "getMethods", new Class<?>[0], new Object[0]);
            if (methods instanceof Collection && !((Collection<?>) methods).isEmpty()) {
                return String.valueOf(((Collection<?>) methods).iterator().next());
            }
        } catch (Throwable ignored) {
        }
        return "ANY";
    }

    private static String schedulesJson() {
        Object context = applicationContext;
        if (context == null) {
            return "{\"context\":false,\"schedules\":[]}";
        }
        StringBuilder out = new StringBuilder("{\"context\":true,\"schedules\":[");
        boolean first = true;
        try {
            Object names = invoke(context, "getBeanDefinitionNames", new Class<?>[0], new Object[0]);
            if (names instanceof String[]) {
                for (String name : (String[]) names) {
                    if (name.startsWith("org.springframework")) {
                        continue;
                    }
                    Object bean;
                    try {
                        bean = invoke(context, "getBean", new Class<?>[]{String.class}, new Object[]{name});
                    } catch (Throwable ignored) {
                        continue;
                    }
                    Class<?> type = bean.getClass();
                    while (type != null && type != Object.class) {
                        for (Method method : type.getDeclaredMethods()) {
                            if (!hasAnnotation(method, "Scheduled")) {
                                continue;
                            }
                            if (!first) out.append(',');
                            first = false;
                            out.append("{\"bean\":\"").append(escape(name))
                                    .append("\",\"className\":\"").append(escape(type.getSimpleName()))
                                    .append("\",\"methodName\":\"").append(escape(method.getName())).append("\"}");
                        }
                        type = type.getSuperclass();
                    }
                }
            }
        } catch (Throwable error) {
            return "{\"context\":true,\"error\":" + jsonString(error.toString()) + ",\"schedules\":[]}";
        }
        return out.append("]}").toString();
    }

    private static String triggerSchedule(String requestBody) {
        try {
            JsonNode request = MAPPER.readTree(requestBody);
            String className = request.path("className").asText();
            String methodName = request.path("methodName").asText();
            String beanName = request.path("bean").asText("");
            Object context = applicationContext;
            if (context == null) {
                return "{\"ok\":false,\"error\":\"Spring context is not ready\"}";
            }
            Object names = invoke(context, "getBeanDefinitionNames", new Class<?>[0], new Object[0]);
            if (names instanceof String[]) {
                for (String name : (String[]) names) {
                    if (!beanName.isEmpty() && !beanName.equals(name)) {
                        continue;
                    }
                    Object bean;
                    try {
                        bean = invoke(context, "getBean", new Class<?>[]{String.class}, new Object[]{name});
                    } catch (Throwable ignored) {
                        continue;
                    }
                    Class<?> type = bean.getClass();
                    while (type != null && type != Object.class) {
                        if (type.getSimpleName().equals(className)) {
                            for (Method method : type.getDeclaredMethods()) {
                                if (method.getName().equals(methodName) && hasAnnotation(method, "Scheduled")) {
                                    if (method.getParameterTypes().length != 0) {
                                        return "{\"ok\":false,\"error\":\"Scheduled method takes parameters\"}";
                                    }
                                    method.setAccessible(true);
                                    Object result = method.invoke(bean);
                                    return "{\"ok\":true,\"result\":" + MAPPER.writeValueAsString(result) + "}";
                                }
                            }
                        }
                        type = type.getSuperclass();
                    }
                }
            }
            return "{\"ok\":false,\"error\":\"Scheduled method not found\"}";
        } catch (InvocationTargetException error) {
            Throwable cause = error.getCause() == null ? error : error.getCause();
            return "{\"ok\":false,\"error\":" + jsonString(cause.toString()) + "}";
        } catch (Throwable error) {
            return "{\"ok\":false,\"error\":" + jsonString(error.toString()) + "}";
        }
    }

    private static String routesJson() {
        Object context = applicationContext;
        if (context == null) {
            return "{\"context\":false,\"routes\":[]}";
        }
        StringBuilder out = new StringBuilder("{\"context\":true,\"routes\":[");
        boolean first = true;
        try {
            Class<?> locator = Class.forName("org.springframework.cloud.gateway.route.RouteDefinitionLocator");
            Object beans = invoke(context, "getBeansOfType", new Class<?>[]{Class.class}, new Object[]{locator});
            if (beans instanceof Map) {
                for (Object bean : ((Map<?, ?>) beans).values()) {
                    Object definitions = invoke(bean, "getRouteDefinitions", new Class<?>[0], new Object[0]);
                    if (!(definitions instanceof Collection)) {
                        continue;
                    }
                    for (Object definition : (Collection<?>) definitions) {
                        if (!first) out.append(',');
                        first = false;
                        Object id = invoke(definition, "getId", new Class<?>[0], new Object[0]);
                        Object uri = invoke(definition, "getUri", new Class<?>[0], new Object[0]);
                        Object predicates = invoke(definition, "getPredicates", new Class<?>[0], new Object[0]);
                        out.append("{\"id\":\"").append(escape(String.valueOf(id)))
                                .append("\",\"uri\":\"").append(escape(String.valueOf(uri)))
                                .append("\",\"predicates\":\"").append(escape(String.valueOf(predicates))).append("\"}");
                    }
                }
            }
        } catch (ClassNotFoundException ignored) {
            return "{\"context\":true,\"routes\":[]}";
        } catch (Throwable error) {
            return "{\"context\":true,\"error\":" + jsonString(error.toString()) + ",\"routes\":[]}";
        }
        return out.append("]}").toString();
    }

    private static boolean hasAnnotation(Method method, String simpleName) {
        for (Annotation annotation : method.getAnnotations()) {
            if (annotation.annotationType().getSimpleName().equals(simpleName)) {
                return true;
            }
        }
        return false;
    }

    private static String invokeController(String requestBody) {
        try {
            JsonNode request = MAPPER.readTree(requestBody);
            String controller = request.path("controller").asText();
            String methodName = request.path("methodName").asText();
            Object context = applicationContext;
            if (context == null) {
                return "{\"ok\":false,\"error\":\"Spring context is not ready\"}";
            }
            Object mapping = invoke(context, "getBean", new Class<?>[]{String.class}, new Object[]{"requestMappingHandlerMapping"});
            Object methods = invoke(mapping, "getHandlerMethods", new Class<?>[0], new Object[0]);
            if (!(methods instanceof Map)) {
                return "{\"ok\":false,\"error\":\"No request mappings\"}";
            }
            for (Object entryObject : ((Map<?, ?>) methods).entrySet()) {
                Map.Entry<?, ?> entry = (Map.Entry<?, ?>) entryObject;
                Object handler = entry.getValue();
                Object beanType = invoke(handler, "getBeanType", new Class<?>[0], new Object[0]);
                Object methodObject = invoke(handler, "getMethod", new Class<?>[0], new Object[0]);
                if (!(beanType instanceof Class) || !(methodObject instanceof Method)) {
                    continue;
                }
                Method method = (Method) methodObject;
                if (!((Class<?>) beanType).getSimpleName().equals(controller) || !method.getName().equals(methodName)) {
                    continue;
                }
                Object bean = invoke(context, "getBean", new Class<?>[]{Class.class}, new Object[]{beanType});
                Object[] args = arguments(request, method);
                method.setAccessible(true);
                Object result = method.invoke(bean, args);
                return "{\"ok\":true,\"result\":" + MAPPER.writeValueAsString(result) + "}";
            }
            return "{\"ok\":false,\"error\":\"No runtime handler for " + escape(controller) + "." + escape(methodName) + "\"}";
        } catch (InvocationTargetException error) {
            Throwable cause = error.getCause() == null ? error : error.getCause();
            return "{\"ok\":false,\"error\":" + jsonString(cause.toString()) + "}";
        } catch (Throwable error) {
            return "{\"ok\":false,\"error\":" + jsonString(error.toString()) + "}";
        }
    }

    private static String invokeMethod(String requestBody) {
        long started = System.nanoTime();
        try {
            JsonNode request = MAPPER.readTree(requestBody);
            String className = request.path("className").asText();
            String methodName = request.path("methodName").asText();
            JsonNode typeNodes = request.path("parameterTypes");
            JsonNode argNodes = request.path("args");
            if (className.isEmpty() || methodName.isEmpty()) {
                return "{\"ok\":false,\"error\":\"className and methodName are required\"}";
            }
            Class<?> type = resolveClass(className);
            if (type == null) {
                return "{\"ok\":false,\"error\":" + jsonString("Class not found: " + className) + "}";
            }
            Method method = findMethod(type, methodName, typeNodes);
            if (method == null) {
                return "{\"ok\":false,\"error\":" + jsonString("Method not found: " + className + "." + methodName) + "}";
            }
            Object bean = null;
            String beanLabel = "";
            if (!Modifier.isStatic(method.getModifiers())) {
                bean = resolveBean(type);
                beanLabel = bean == null ? "" : bean.getClass().getName();
                if (bean == null) {
                    if (type.isInterface() || Modifier.isAbstract(type.getModifiers())) {
                        return "{\"ok\":false,\"error\":" + jsonString("No Spring bean or concrete class for " + className) + "}";
                    }
                    java.lang.reflect.Constructor<?> constructor = type.getDeclaredConstructor();
                    constructor.setAccessible(true);
                    bean = constructor.newInstance();
                    beanLabel = className;
                }
            }
            Class<?>[] parameterTypes = method.getParameterTypes();
            Object[] args = new Object[parameterTypes.length];
            for (int i = 0; i < parameterTypes.length; i++) {
                JsonNode node = i < argNodes.size() ? argNodes.get(i) : null;
                args[i] = convertJson(node, parameterTypes[i]);
            }
            method.setAccessible(true);
            Object result = method.invoke(bean, args);
            long elapsed = (System.nanoTime() - started) / 1_000_000L;
            String resultJson;
            try {
                resultJson = MAPPER.writeValueAsString(result);
            } catch (Throwable ignored) {
                resultJson = jsonString(String.valueOf(result));
            }
            return "{\"ok\":true,\"className\":" + jsonString(className)
                    + ",\"methodName\":" + jsonString(methodName)
                    + ",\"bean\":" + jsonString(beanLabel)
                    + ",\"ms\":" + elapsed
                    + ",\"result\":" + resultJson + "}";
        } catch (InvocationTargetException error) {
            Throwable cause = error.getCause() == null ? error : error.getCause();
            return "{\"ok\":false,\"error\":" + jsonString(cause.toString()) + "}";
        } catch (Throwable error) {
            return "{\"ok\":false,\"error\":" + jsonString(error.toString()) + "}";
        }
    }

    private static Class<?> resolveClass(String name) {
        // The application's context loader sees the same Class objects as its
        // beans; the system loader can contain a second copy of application
        // classes and make getBean(Class) miss every bean.
        Object context = applicationContext;
        if (context != null) {
            try {
                Object loader = invoke(context, "getClassLoader", new Class<?>[0], new Object[0]);
                if (loader instanceof ClassLoader) return Class.forName(name, true, (ClassLoader) loader);
            } catch (Throwable ignored) {
            }
            try {
                ClassLoader loader = context.getClass().getClassLoader();
                if (loader != null) return Class.forName(name, true, loader);
            } catch (Throwable ignored) {
            }
        }
        ClassLoader loader = Thread.currentThread().getContextClassLoader();
        if (loader != null) {
            try {
                return Class.forName(name, true, loader);
            } catch (Throwable ignored) {
            }
        }
        try {
            return Class.forName(name);
        } catch (Throwable ignored) {
            return null;
        }
    }

    private static Object resolveBean(Class<?> type) {
        Object context = applicationContext;
        if (context == null) return null;
        try {
            return invoke(context, "getBean", new Class<?>[]{Class.class}, new Object[]{type});
        } catch (Throwable ignored) {
        }
        try {
            Object beans = invoke(context, "getBeansOfType", new Class<?>[]{Class.class}, new Object[]{type});
            if (beans instanceof Map && !((Map<?, ?>) beans).isEmpty()) {
                return ((Map<?, ?>) beans).values().iterator().next();
            }
        } catch (Throwable ignored) {
        }
        // Spring Boot's LaunchedURLClassLoader can make getBean(Class) fail for
        // classes resolved through the thread context loader. Scanning bean
        // definitions still finds the instance by type without instantiation.
        try {
            Object names = invoke(context, "getBeanDefinitionNames", new Class<?>[0], new Object[0]);
            if (names instanceof String[]) {
                for (String name : (String[]) names) {
                    try {
                        Object bean = invoke(context, "getBean", new Class<?>[]{String.class}, new Object[]{name});
                        if (bean != null && type.isInstance(bean)) return bean;
                    } catch (Throwable ignored) {
                    }
                }
            }
        } catch (Throwable ignored) {
        }
        return null;
    }

    private static Method findMethod(Class<?> type, String name, JsonNode typeNodes) {
        Method match = null;
        for (Class<?> current = type; current != null; current = current.getSuperclass()) {
            Method[] methods;
            try {
                methods = current.getDeclaredMethods();
            } catch (Throwable ignored) {
                continue;
            }
            for (Method method : methods) {
                if (!method.getName().equals(name)) continue;
                if (!parameterTypesMatch(method, typeNodes)) continue;
                if (match != null && match.getParameterCount() == method.getParameterCount()) {
                    return null;
                }
                match = method;
            }
            if (match != null) return match;
        }
        try {
            for (Method method : type.getMethods()) {
                if (!method.getName().equals(name)) continue;
                if (!parameterTypesMatch(method, typeNodes)) continue;
                if (match != null && match.getParameterCount() == method.getParameterCount()) {
                    return null;
                }
                match = method;
            }
        } catch (Throwable ignored) {
        }
        return match;
    }

    private static boolean parameterTypesMatch(Method method, JsonNode typeNodes) {
        int count = method.getParameterCount();
        if (typeNodes != null && typeNodes.isArray() && typeNodes.size() > 0) {
            if (typeNodes.size() != count) return false;
            Class<?>[] actual = method.getParameterTypes();
            for (int i = 0; i < count; i++) {
                String wanted = typeNodes.get(i).asText().replaceAll("<.*>", "").replace("[]", "").trim();
                String simple = actual[i].getSimpleName();
                String raw = actual[i].getName();
                String wantedSimple = wanted.contains(".") ? wanted.substring(wanted.lastIndexOf('.') + 1) : wanted;
                if (!wanted.isEmpty() && !wantedSimple.equals(simple) && !raw.equals(wanted) && !raw.endsWith("." + wantedSimple)) {
                    return false;
                }
            }
            return true;
        }
        return count >= 0;
    }

    private static Object convertJson(JsonNode node, Class<?> type) throws Exception {
        if (node == null || node.isNull() || node.isMissingNode()) {
            if (type.isPrimitive()) return primitiveDefault(type);
            return null;
        }
        if (type == String.class) return node.isTextual() ? node.asText() : node.toString();
        if (type == char.class || type == Character.class) {
            String text = node.asText("");
            return text.isEmpty() ? null : text.charAt(0);
        }
        if (node.isTextual()) return convert(node.asText(), type);
        return MAPPER.convertValue(node, type);
    }

    private static Object[] arguments(JsonNode request, Method method) throws Exception {
        Class<?>[] types = method.getParameterTypes();
        Object[] args = new Object[types.length];
        JsonNode query = request.path("query");
        JsonNode pathVariables = request.path("pathVariables");
        JsonNode headers = request.path("headers");
        String body = request.path("body").asText("");
        for (int i = 0; i < types.length; i++) {
            Class<?> type = types[i];
            String typeName = type.getName();
            if (typeName.startsWith("javax.servlet.") || typeName.startsWith("jakarta.servlet.")) {
                throw new IllegalArgumentException("Servlet request/response parameters are not supported by reflection mode");
            }
            Annotation[] annotations = method.getParameterAnnotations()[i];
            String kind = null;
            String name = null;
            String defaultValue = null;
            boolean required = true;
            for (Annotation annotation : annotations) {
                String simple = annotation.annotationType().getSimpleName();
                if ("RequestBody".equals(simple)) {
                    kind = "body";
                    required = annotationValue(annotation, "required", true);
                } else if ("PathVariable".equals(simple)) {
                    kind = "path";
                    name = annotationValue(annotation, "value", annotationValue(annotation, "name", ""));
                    required = annotationValue(annotation, "required", true);
                } else if ("RequestParam".equals(simple)) {
                    kind = "query";
                    name = annotationValue(annotation, "value", annotationValue(annotation, "name", ""));
                    defaultValue = annotationValue(annotation, "defaultValue", null);
                    required = annotationValue(annotation, "required", true);
                } else if ("RequestHeader".equals(simple)) {
                    kind = "header";
                    name = annotationValue(annotation, "value", annotationValue(annotation, "name", ""));
                    defaultValue = annotationValue(annotation, "defaultValue", null);
                    required = annotationValue(annotation, "required", true);
                }
            }
            if ("body".equals(kind)) {
                if (body == null || body.isEmpty()) {
                    args[i] = null;
                } else if (type == String.class) {
                    args[i] = body;
                } else if (type == byte[].class) {
                    args[i] = body.getBytes(java.nio.charset.StandardCharsets.UTF_8);
                } else {
                    args[i] = MAPPER.readValue(body, type);
                }
                continue;
            }
            if (name == null || name.isEmpty()) {
                name = method.getParameters()[i].getName();
            }
            JsonNode source = "path".equals(kind) ? pathVariables : "header".equals(kind) ? headers : query;
            JsonNode value = source.path(name);
            if (value.isMissingNode() || value.isNull()) {
                if (defaultValue != null && !defaultValue.isEmpty()) {
                    args[i] = convert(defaultValue, type);
                } else if (required && kind != null) {
                    throw new IllegalArgumentException("Missing " + kind + " parameter " + name);
                } else {
                    args[i] = type.isPrimitive() ? primitiveDefault(type) : null;
                }
            } else {
                args[i] = convert(value.isTextual() ? value.asText() : value.toString(), type);
            }
        }
        return args;
    }

    private static Object convert(String value, Class<?> type) throws Exception {
        if (type == String.class) return value;
        if (type == int.class || type == Integer.class) return Integer.parseInt(value);
        if (type == long.class || type == Long.class) return Long.parseLong(value);
        if (type == boolean.class || type == Boolean.class) return Boolean.parseBoolean(value);
        if (type == double.class || type == Double.class) return Double.parseDouble(value);
        if (type == float.class || type == Float.class) return Float.parseFloat(value);
        if (type == short.class || type == Short.class) return Short.parseShort(value);
        if (type.isEnum()) {
            @SuppressWarnings({"unchecked", "rawtypes"})
            Object constant = Enum.valueOf((Class<? extends Enum>) type, value);
            return constant;
        }
        return MAPPER.readValue(value, type);
    }

    private static Object primitiveDefault(Class<?> type) {
        if (type == int.class) return 0;
        if (type == long.class) return 0L;
        if (type == boolean.class) return false;
        if (type == double.class) return 0d;
        if (type == float.class) return 0f;
        if (type == short.class) return (short) 0;
        if (type == byte.class) return (byte) 0;
        if (type == char.class) return ' ';
        return null;
    }

    @SuppressWarnings("unchecked")
    private static <T> T annotationValue(Annotation annotation, String key, T fallback) {
        try {
            Object value = annotation.annotationType().getMethod(key).invoke(annotation);
            if (value instanceof String && ((String) value).isEmpty() && fallback != null) {
                return fallback;
            }
            return value == null ? fallback : (T) value;
        } catch (Throwable ignored) {
            return fallback;
        }
    }

    private static String jsonString(String value) {
        return "\"" + escape(value) + "\"";
    }

    private static boolean authorized(HttpExchange exchange) {
        String header = exchange.getRequestHeaders().getFirst("X-OpenIDEA-Token");
        return token != null && token.equals(header);
    }

    private static void unauthorized(HttpExchange exchange) throws IOException {
        byte[] body = "{\"ok\":false,\"error\":\"unauthorized\"}".getBytes(StandardCharsets.UTF_8);
        exchange.getResponseHeaders().set("Content-Type", "application/json");
        exchange.sendResponseHeaders(401, body.length);
        try (OutputStream out = exchange.getResponseBody()) {
            out.write(body);
        }
    }

    private static final class MethodInvokeHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            byte[] response = invokeMethod(readAll(exchange.getRequestBody())).getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, response.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(response);
            }
        }
    }

    private static final class InvokeHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            String body = readAll(exchange.getRequestBody());
            byte[] response = invokeController(body).getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, response.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(response);
            }
        }
    }

    private static String readAll(InputStream input) throws IOException {
        ByteArrayOutputStream buffer = new ByteArrayOutputStream();
        byte[] data = new byte[4096];
        int read;
        while ((read = input.read(data)) != -1) {
            buffer.write(data, 0, read);
        }
        return new String(buffer.toByteArray(), StandardCharsets.UTF_8);
    }

    private static int serverPort() {
        Object context = applicationContext;
        if (context == null) {
            return 0;
        }
        try {
            Object environment = invoke(context, "getEnvironment", new Class<?>[0], new Object[0]);
            Object port = invoke(environment, "getProperty", new Class<?>[]{String.class}, new Object[]{"local.server.port"});
            if (port != null && !String.valueOf(port).isEmpty()) {
                return Integer.parseInt(String.valueOf(port));
            }
        } catch (Throwable ignored) {
        }
        try {
            Object server = invoke(context, "getWebServer", new Class<?>[0], new Object[0]);
            Object port = invoke(server, "getPort", new Class<?>[0], new Object[0]);
            return port instanceof Number ? ((Number) port).intValue() : 0;
        } catch (Throwable ignored) {
            return 0;
        }
    }

    private static String contextPath() {
        Object context = applicationContext;
        if (context == null) {
            return "";
        }
        try {
            Object environment = invoke(context, "getEnvironment", new Class<?>[0], new Object[0]);
            Object path = invoke(environment, "getProperty", new Class<?>[]{String.class}, new Object[]{"server.servlet.context-path"});
            if (path == null || String.valueOf(path).isEmpty()) {
                path = invoke(environment, "getProperty", new Class<?>[]{String.class}, new Object[]{"server.context-path"});
            }
            return path == null ? "" : String.valueOf(path);
        } catch (Throwable ignored) {
            return "";
        }
    }

    private static long pid() {
        try {
            String name = java.lang.management.ManagementFactory.getRuntimeMXBean().getName();
            return Long.parseLong(name.substring(0, name.indexOf('@')));
        } catch (Throwable ignored) {
            return -1L;
        }
    }

    private static String escape(String value) {
        return value == null ? "" : value.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    private static final class MappingsHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            byte[] body = mappingsJson().getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, body.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(body);
            }
        }
    }

    private static final class SchedulesHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            byte[] body = schedulesJson().getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, body.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(body);
            }
        }
    }

    private static final class TriggerScheduleHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            byte[] body = triggerSchedule(readAll(exchange.getRequestBody())).getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, body.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(body);
            }
        }
    }

    private static final class RoutesHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            byte[] body = routesJson().getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, body.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(body);
            }
        }
    }

    private static synchronized void installSqlTrace(Object context) {
        if (sqlTraceInstalled) {
            return;
        }
        try {
            Class<?> factory = Class.forName("org.apache.ibatis.session.SqlSessionFactory");
            Object beans = invoke(context, "getBeansOfType", new Class<?>[]{Class.class}, new Object[]{factory});
            boolean added = false;
            if (beans instanceof java.util.Map) {
                for (Object bean : ((java.util.Map<?, ?>) beans).values()) {
                    Object configuration = invoke(bean, "getConfiguration", new Class<?>[0], new Object[0]);
                    invoke(configuration, "addInterceptor", new Class<?>[]{Class.forName("org.apache.ibatis.plugin.Interceptor")},
                            new Object[]{new SqlTraceInterceptor()});
                    added = true;
                }
            }
            sqlTraceInstalled = added;
        } catch (Throwable ignored) {
        }
    }

    private static final class TraceHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            if ("POST".equalsIgnoreCase(exchange.getRequestMethod())) {
                String body = readAll(exchange.getRequestBody());
                try {
                    JsonNode request = MAPPER.readTree(body);
                    boolean enabled = request.path("enabled").asBoolean(false);
                    TraceRecorder.setSettings(request.path("maxDepth").asInt(8),
                            request.path("includeSql").asBoolean(true));
                    if (request.has("targets")) {
                        java.util.List<TraceInstaller.Target> targets = new java.util.ArrayList<TraceInstaller.Target>();
                        if (enabled) {
                            JsonNode entries = request.path("targets");
                            if (entries.isArray()) {
                                for (JsonNode entry : entries) {
                                    targets.add(new TraceInstaller.Target(entry.path("type").asText(""),
                                            entry.path("method").asText("")));
                                }
                            }
                        }
                        TraceInstaller.configure(instrumentation, applicationContext, targets);
                    }
                    TraceRecorder.setEnabled(enabled);
                } catch (Throwable ignored) {
                }
            }
            byte[] response = TraceRecorder.framesJson().getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, response.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(response);
            }
        }
    }

    private static String mcp(String requestBody) {
        JsonNode id = null;
        try {
            JsonNode request = MAPPER.readTree(requestBody);
            id = request.path("id");
            String method = request.path("method").asText("");
            if ("initialize".equals(method)) {
                return mcpResult(id, "{\"protocolVersion\":\"2024-11-05\",\"serverInfo\":{\"name\":\"openidea-spring-agent\",\"version\":\"0.1.0\"},\"capabilities\":{\"tools\":{}}}");
            }
            if ("tools/list".equals(method)) {
                return mcpResult(id, toolsJson());
            }
            if ("tools/call".equals(method)) {
                JsonNode params = request.path("params");
                String name = params.path("name").asText("");
                JsonNode arguments = params.path("arguments");
                String text;
                try {
                    text = callTool(name, arguments);
                } catch (Throwable error) {
                    return mcpError(id, -32000, error.toString());
                }
                return mcpResult(id, "{\"content\":[{\"type\":\"text\",\"text\":" + jsonString(text) + "}],\"isError\":false}");
            }
            return mcpError(id, -32601, "Unknown method: " + method);
        } catch (Throwable error) {
            return mcpError(id, -32700, error.toString());
        }
    }

    private static String toolsJson() {
        return "{\"tools\":["
                + tool("getContext", "Runtime context summary and counts", "{}") + ","
                + tool("getObjectInstances", "List Spring bean instances", "{\"type\":{\"type\":\"string\",\"description\":\"optional class/simple-name filter\"}}") + ","
                + tool("callMethod", "Invoke a bean method with JSON arguments", "{\"bean\":{\"type\":\"string\"},\"method\":{\"type\":\"string\"},\"args\":{\"type\":\"array\",\"items\":{}}}") + ","
                + tool("getMappings", "Runtime Spring MVC mappings", "{}") + ","
                + tool("getSchedules", "Runtime @Scheduled methods", "{}") + ","
                + tool("getRoutes", "Runtime Gateway routes", "{}") + ","
                + tool("getSql", "Recent MyBatis SQL frames", "{}") + ","
                + tool("getTrace", "Recent method trace frames", "{}") + ","
                + tool("runScript", "Compile and run a Java body", "{\"code\":{\"type\":\"string\"},\"timeoutMs\":{\"type\":\"integer\"}}")
                + "]}";
    }

    private static String tool(String name, String description, String properties) {
        return "{\"name\":\"" + escape(name) + "\",\"description\":\"" + escape(description)
                + "\",\"inputSchema\":{\"type\":\"object\",\"properties\":" + properties + "}}";
    }

    private static String callTool(String name, JsonNode arguments) throws Exception {
        Object context = applicationContext;
        if ("getContext".equals(name)) {
            int mappings = 0;
            try {
                JsonNode value = MAPPER.readTree(mappingsJson());
                mappings = value.path("mappings").size();
            } catch (Throwable ignored) {
            }
            return "{\"context\":" + (context != null) + ",\"contextClass\":\""
                    + (context == null ? "" : escape(context.getClass().getName())) + "\",\"serverPort\":" + serverPort()
                    + ",\"mappings\":" + mappings + "}";
        }
        if ("getObjectInstances".equals(name)) {
            return objectInstances(arguments.path("type").asText(""));
        }
        if ("callMethod".equals(name)) {
            return callBeanMethod(arguments.path("bean").asText(""), arguments.path("method").asText(""), arguments.path("args"));
        }
        if ("getMappings".equals(name)) return mappingsJson();
        if ("getSchedules".equals(name)) return schedulesJson();
        if ("getRoutes".equals(name)) return routesJson();
        if ("getSql".equals(name)) return SqlRecorder.framesJson();
        if ("getTrace".equals(name)) return TraceRecorder.framesJson();
        if ("runScript".equals(name)) {
            return ScriptRunner.run(arguments.path("code").asText(""), arguments.path("timeoutMs").asLong(10_000L));
        }
        throw new IllegalArgumentException("Unknown tool: " + name);
    }

    private static String objectInstances(String filter) {
        Object context = applicationContext;
        if (context == null) {
            return "{\"context\":false,\"instances\":[]}";
        }
        StringBuilder out = new StringBuilder("{\"context\":true,\"instances\":[");
        boolean first = true;
        try {
            Object names = invoke(context, "getBeanDefinitionNames", new Class<?>[0], new Object[0]);
            if (names instanceof String[]) {
                for (String beanName : (String[]) names) {
                    Object bean;
                    try {
                        bean = invoke(context, "getBean", new Class<?>[]{String.class}, new Object[]{beanName});
                    } catch (Throwable ignored) {
                        continue;
                    }
                    Class<?> type = bean.getClass();
                    String simple = type.getSimpleName();
                    if (!filter.isEmpty() && !beanName.contains(filter)
                            && !type.getName().contains(filter) && !simple.contains(filter)) {
                        continue;
                    }
                    if (!first) out.append(',');
                    first = false;
                    out.append("{\"bean\":\"").append(escape(beanName))
                            .append("\",\"className\":\"").append(escape(type.getName()))
                            .append("\",\"simpleName\":\"").append(escape(simple)).append("\"}");
                }
            }
        } catch (Throwable error) {
            return "{\"context\":true,\"error\":" + jsonString(error.toString()) + ",\"instances\":[]}";
        }
        return out.append("]}").toString();
    }

    private static String callBeanMethod(String beanName, String methodName, JsonNode args) throws Exception {
        Object context = applicationContext;
        if (context == null) {
            return "{\"ok\":false,\"error\":\"Spring context is not ready\"}";
        }
        Object bean = invoke(context, "getBean", new Class<?>[]{String.class}, new Object[]{beanName});
        int count = args.isArray() ? args.size() : 0;
        for (Method method : bean.getClass().getMethods()) {
            if (!method.getName().equals(methodName) || method.getParameterTypes().length != count) {
                continue;
            }
            Class<?>[] types = method.getParameterTypes();
            Object[] values = new Object[count];
            for (int i = 0; i < count; i++) {
                values[i] = MAPPER.convertValue(args.get(i), types[i]);
            }
            method.setAccessible(true);
            Object result = method.invoke(bean, values);
            return "{\"ok\":true,\"result\":" + MAPPER.writeValueAsString(result) + "}";
        }
        return "{\"ok\":false,\"error\":\"No method " + escape(methodName) + " with " + count + " arguments\"}";
    }

    private static String mcpResult(JsonNode id, String result) {
        return "{\"jsonrpc\":\"2.0\",\"id\":" + String.valueOf(id) + ",\"result\":" + result + "}";
    }

    private static String mcpError(JsonNode id, int code, String message) {
        return "{\"jsonrpc\":\"2.0\",\"id\":" + String.valueOf(id)
                + ",\"error\":{\"code\":" + code + ",\"message\":" + jsonString(message) + "}}";
    }

    static Object contextObject() {
        return applicationContext;
    }

    private static final class McpHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            if (!"POST".equalsIgnoreCase(exchange.getRequestMethod())) {
                byte[] body = "{\"error\":\"POST JSON-RPC\"}".getBytes(StandardCharsets.UTF_8);
                exchange.sendResponseHeaders(405, body.length);
                try (OutputStream out = exchange.getResponseBody()) {
                    out.write(body);
                }
                return;
            }
            byte[] response = mcp(readAll(exchange.getRequestBody())).getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, response.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(response);
            }
        }
    }

    private static final class ScriptHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            if ("POST".equalsIgnoreCase(exchange.getRequestMethod())) {
                try {
                    JsonNode request = MAPPER.readTree(readAll(exchange.getRequestBody()));
                    ScriptRunner.setEnabled(request.path("enabled").asBoolean(false));
                } catch (Throwable ignored) {
                }
            }
            byte[] response = ScriptRunner.statusJson().getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, response.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(response);
            }
        }
    }

    private static final class ScriptRunHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            String response;
            try {
                JsonNode request = MAPPER.readTree(readAll(exchange.getRequestBody()));
                response = ScriptRunner.run(request.path("code").asText(""), request.path("timeoutMs").asLong(10_000L));
            } catch (Throwable error) {
                response = "{\"ok\":false,\"error\":\"invalid script request\"}";
            }
            byte[] body = response.getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, body.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(body);
            }
        }
    }

    private static final class SqlHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            if (!authorized(exchange)) {
                unauthorized(exchange);
                return;
            }
            if ("POST".equalsIgnoreCase(exchange.getRequestMethod())) {
                try {
                    JsonNode request = MAPPER.readTree(readAll(exchange.getRequestBody()));
                    SqlRecorder.setEnabled(request.path("enabled").asBoolean(false));
                } catch (Throwable ignored) {
                }
            }
            byte[] response = SqlRecorder.framesJson().getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, response.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(response);
            }
        }
    }

    private static final class HealthHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            byte[] body = ("{\"ok\":true,\"context\":" + (applicationContext != null) + "}").getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, body.length);
            try (OutputStream out = exchange.getResponseBody()) {
                out.write(body);
            }
        }
    }
}
