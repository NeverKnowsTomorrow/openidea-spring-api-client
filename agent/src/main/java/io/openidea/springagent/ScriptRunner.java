package io.openidea.springagent;

import com.fasterxml.jackson.databind.ObjectMapper;

import javax.tools.Diagnostic;
import javax.tools.DiagnosticCollector;
import javax.tools.FileObject;
import javax.tools.ForwardingJavaFileManager;
import javax.tools.JavaCompiler;
import javax.tools.JavaFileManager;
import javax.tools.JavaFileObject;
import javax.tools.SimpleJavaFileObject;
import javax.tools.StandardJavaFileManager;
import javax.tools.ToolProvider;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.net.URI;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.Callable;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;

/** Compiles a small Java body in memory and runs it in the application JVM. */
public final class ScriptRunner {
    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static volatile boolean enabled;

    private ScriptRunner() {
    }

    public static boolean isEnabled() {
        return enabled;
    }

    public static void setEnabled(boolean value) {
        enabled = value;
    }

    public static String statusJson() {
        return "{\"enabled\":" + enabled + "}";
    }

    public static String run(String code, long timeoutMs) {
        if (!enabled) {
            return error("Java scripts are disabled");
        }
        if (code == null || code.trim().isEmpty()) {
            return error("Script is empty");
        }
        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        if (compiler == null) {
            return error("No Java compiler in this JVM (a JDK is required)");
        }
        long limit = Math.max(1_000L, Math.min(timeoutMs <= 0 ? 10_000L : timeoutMs, 60_000L));
        long started = System.nanoTime();
        try {
            String source = buildSource(code);
            DiagnosticCollector<JavaFileObject> diagnostics = new DiagnosticCollector<JavaFileObject>();
            StandardJavaFileManager standard = compiler.getStandardFileManager(diagnostics, null, StandardCharsets.UTF_8);
            MemoryFileManager files = new MemoryFileManager(standard);
            Iterable<? extends JavaFileObject> units = Arrays.<JavaFileObject>asList(new SourceFile(source));
            List<String> options = Arrays.asList("-classpath", classpath(), "-proc:none");
            boolean ok = compiler.getTask(null, files, diagnostics, options, null, units).call();
            if (!ok) {
                StringBuilder message = new StringBuilder("Compilation failed\n");
                for (Diagnostic<? extends JavaFileObject> diagnostic : diagnostics.getDiagnostics()) {
                    message.append("line ").append(diagnostic.getLineNumber()).append(": ")
                            .append(diagnostic.getMessage(null)).append('\n');
                }
                return error(message.toString());
            }
            byte[] bytes = files.bytes("OpenIdeaScript");
            if (bytes == null) {
                return error("Compiler produced no class");
            }
            ClassLoader parent = Thread.currentThread().getContextClassLoader();
            MemoryClassLoader loader = new MemoryClassLoader(parent, bytes);
            Class<?> script = loader.loadClass("OpenIdeaScript");
            ExecutorService executor = Executors.newSingleThreadExecutor(runnable -> {
                Thread thread = new Thread(runnable, "openidea-script");
                thread.setDaemon(true);
                return thread;
            });
            try {
                Future<Object> future = executor.submit(new Callable<Object>() {
                    @Override
                    public Object call() throws Exception {
                        return script.getMethod("run").invoke(null);
                    }
                });
                Object result = future.get(limit, TimeUnit.MILLISECONDS);
                long elapsed = (System.nanoTime() - started) / 1_000_000L;
                return "{\"ok\":true,\"result\":" + json(result) + ",\"ms\":" + elapsed + "}";
            } finally {
                executor.shutdownNow();
            }
        } catch (java.util.concurrent.TimeoutException error) {
            return error("Script timed out after " + limit + " ms");
        } catch (Throwable error) {
            Throwable cause = error;
            while ((cause instanceof java.util.concurrent.ExecutionException
                    || cause instanceof java.lang.reflect.InvocationTargetException)
                    && cause.getCause() != null) {
                cause = cause.getCause();
            }
            return error(cause.toString());
        }
    }

    private static String buildSource(String code) {
        return "import java.util.*;\n"
                + "import java.util.stream.*;\n"
                + "public final class OpenIdeaScript {\n"
                + "  public static Object run() throws Throwable {\n"
                + code + "\n"
                + "  }\n"
                + "}\n";
    }

    private static String classpath() {
        StringBuilder out = new StringBuilder(System.getProperty("java.class.path", ""));
        ClassLoader loader = Thread.currentThread().getContextClassLoader();
        while (loader != null) {
            if (loader instanceof URLClassLoader) {
                for (URL url : ((URLClassLoader) loader).getURLs()) {
                    String path = url.getPath();
                    if (path.endsWith(".jar") || path.contains("/classes/") || path.endsWith("/classes")) {
                        if (out.length() > 0) out.append(java.io.File.pathSeparator);
                        out.append(path);
                    }
                }
            }
            loader = loader.getParent();
        }
        try {
            java.security.CodeSource source = SpringAgent.class.getProtectionDomain().getCodeSource();
            if (source != null && source.getLocation() != null) {
                out.append(java.io.File.pathSeparator).append(new java.io.File(source.getLocation().toURI()).getPath());
            }
        } catch (Throwable ignored) {
        }
        return out.toString();
    }

    private static String json(Object value) {
        try {
            return MAPPER.writeValueAsString(value);
        } catch (Throwable error) {
            return "\"" + String.valueOf(value).replace("\\", "\\\\").replace("\"", "\\\"") + "\"";
        }
    }

    private static String error(String message) {
        try {
            return "{\"ok\":false,\"error\":" + MAPPER.writeValueAsString(message) + "}";
        } catch (Throwable ignored) {
            return "{\"ok\":false,\"error\":\"script failed\"}";
        }
    }

    private static final class SourceFile extends SimpleJavaFileObject {
        private final String source;

        SourceFile(String source) {
            super(URI.create("string:///OpenIdeaScript.java"), Kind.SOURCE);
            this.source = source;
        }

        @Override
        public CharSequence getCharContent(boolean ignoreEncodingErrors) {
            return source;
        }
    }

    private static final class ByteFile extends SimpleJavaFileObject {
        private final ByteArrayOutputStream out = new ByteArrayOutputStream();

        ByteFile(String name, Kind kind) {
            super(URI.create("bytes:///" + name.replace('.', '/') + kind.extension), kind);
        }

        @Override
        public OutputStream openOutputStream() {
            return out;
        }

        byte[] bytes() {
            return out.toByteArray();
        }
    }

    private static final class MemoryFileManager extends ForwardingJavaFileManager<StandardJavaFileManager> {
        private final Map<String, ByteFile> outputs = new LinkedHashMap<String, ByteFile>();

        MemoryFileManager(StandardJavaFileManager delegate) {
            super(delegate);
        }

        @Override
        public JavaFileObject getJavaFileForOutput(JavaFileManager.Location location, String className,
                                                   JavaFileObject.Kind kind, FileObject sibling) throws IOException {
            ByteFile file = new ByteFile(className, kind);
            outputs.put(className, file);
            return file;
        }

        byte[] bytes(String className) {
            ByteFile file = outputs.get(className);
            return file == null ? null : file.bytes();
        }
    }

    private static final class MemoryClassLoader extends ClassLoader {
        private final byte[] bytes;

        MemoryClassLoader(ClassLoader parent, byte[] bytes) {
            super(parent);
            this.bytes = bytes;
        }

        @Override
        protected Class<?> findClass(String name) throws ClassNotFoundException {
            if ("OpenIdeaScript".equals(name)) {
                return defineClass(name, bytes, 0, bytes.length);
            }
            throw new ClassNotFoundException(name);
        }
    }
}
