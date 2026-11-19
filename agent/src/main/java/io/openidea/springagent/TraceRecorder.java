package io.openidea.springagent;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Deque;
import java.util.List;

/** Per-entry method trees; the advice only pays a volatile read when tracing is off. */
public final class TraceRecorder {
    private static final int MAX_TREES = 50;
    private static final int MAX_FRAMES = 5000;
    private static final int MAX_ARGS = 10;
    private static final int MAX_ARG_CHARS = 256;
    private static final Deque<Frame> TREES = new ArrayDeque<Frame>();
    private static final ThreadLocal<Deque<Entry>> STACK = new ThreadLocal<Deque<Entry>>() {
        @Override
        protected Deque<Entry> initialValue() {
            return new ArrayDeque<Entry>();
        }
    };
    private static volatile boolean enabled;
    private static volatile int maxDepth = 8;
    private static volatile boolean includeSql;
    private static volatile long sequence;

    private TraceRecorder() {
    }

    public static void setEnabled(boolean value) {
        enabled = value;
        if (!value) {
            STACK.remove();
        }
    }

    public static synchronized void setSettings(int depth, boolean sql) {
        maxDepth = Math.max(1, Math.min(100, depth));
        includeSql = sql;
    }

    public static boolean isTracing() {
        return enabled && includeSql;
    }

    public static void addSql(String database, String sql, String parameter, long durationMs, String error) {
        if (!enabled || !includeSql) {
            return;
        }
        Deque<Entry> stack = STACK.get();
        if (stack.isEmpty()) {
            return;
        }
        Frame parent = stack.peek().frame;
        if (parent.depth + 1 > maxDepth) {
            return;
        }
        Frame frame = new Frame("sql", database, parent.depth + 1, Thread.currentThread().getName(), Collections.emptyList());
        frame.kind = "sql";
        frame.sql = sql;
        frame.parameter = parameter;
        frame.durationMs = durationMs;
        frame.error = error == null ? "" : error;
        synchronized (parent.children) {
            parent.children.add(frame);
        }
    }

    public static void enter(String type, String method, String descriptor, Object[] arguments) {
        if (!enabled) {
            return;
        }
        Deque<Entry> stack = STACK.get();
        int depth = stack.size();
        Frame frame = new Frame(type, method, depth, Thread.currentThread().getName(), strings(arguments));
        stack.push(new Entry(frame));
    }

    public static void exit(Throwable error) {
        if (!enabled) {
            return;
        }
        Deque<Entry> stack = STACK.get();
        Entry entry = stack.poll();
        if (entry == null) {
            return;
        }
        long now = System.nanoTime();
        Frame frame = entry.frame;
        frame.durationMs = (now - entry.started) / 1_000_000L;
        frame.error = error == null ? "" : String.valueOf(error);
        if (!stack.isEmpty() && frame.depth <= maxDepth) {
            stack.peek().frame.children.add(frame);
            return;
        }
        if (frame.depth <= maxDepth) {
            synchronized (TREES) {
                frame.seq = ++sequence;
                TREES.addFirst(frame);
                while (TREES.size() > MAX_TREES || count(TREES) > MAX_FRAMES) {
                    TREES.removeLast();
                }
            }
        }
    }

    private static int count(Deque<Frame> trees) {
        int total = 0;
        for (Frame tree : trees) {
            total += count(tree);
        }
        return total;
    }

    private static int count(Frame frame) {
        int total = 1;
        for (Frame child : frame.children) {
            total += count(child);
        }
        return total;
    }

    private static List<String> strings(Object[] values) {
        if (values == null || values.length == 0) {
            return Collections.emptyList();
        }
        List<String> out = new ArrayList<String>();
        for (int i = 0; i < values.length && i < MAX_ARGS; i++) {
            String value;
            try {
                value = String.valueOf(values[i]);
            } catch (Throwable ignored) {
                value = "<unprintable>";
            }
            if (value.length() > MAX_ARG_CHARS) {
                value = value.substring(0, MAX_ARG_CHARS) + "…";
            }
            out.add(value);
        }
        return out;
    }

    public static String framesJson() {
        List<Frame> copy;
        synchronized (TREES) {
            copy = new ArrayList<Frame>(TREES);
        }
        StringBuilder out = new StringBuilder("{\"enabled\":").append(enabled)
                .append(",\"seq\":").append(sequence)
                .append(",\"trees\":[");
        for (int i = 0; i < copy.size(); i++) {
            if (i > 0) {
                out.append(',');
            }
            appendFrame(out, copy.get(i));
        }
        return out.append("]}").toString();
    }

    private static void appendFrame(StringBuilder out, Frame frame) {
        out.append("{\"kind\":\"").append(escape(frame.kind))
                .append("\",\"type\":\"").append(escape(frame.type))
                .append("\",\"method\":\"").append(escape(frame.method))
                .append("\",\"sql\":\"").append(escape(frame.sql))
                .append("\",\"parameter\":\"").append(escape(frame.parameter))
                .append("\",\"depth\":").append(frame.depth)
                .append(",\"durationMs\":").append(frame.durationMs)
                .append(",\"thread\":\"").append(escape(frame.thread))
                .append("\",\"error\":\"").append(escape(frame.error))
                .append("\",\"args\":[");
        for (int i = 0; i < frame.args.size(); i++) {
            if (i > 0) {
                out.append(',');
            }
            out.append('"').append(escape(frame.args.get(i))).append('"');
        }
        out.append("],\"children\":[");
        for (int i = 0; i < frame.children.size(); i++) {
            if (i > 0) {
                out.append(',');
            }
            appendFrame(out, frame.children.get(i));
        }
        out.append("]}");
    }

    public static void clear() {
        synchronized (TREES) {
            TREES.clear();
        }
    }

    private static String escape(String value) {
        if (value == null) {
            return "";
        }
        return value.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n");
    }

    private static final class Entry {
        final Frame frame;
        final long started = System.nanoTime();

        Entry(Frame frame) {
            this.frame = frame;
        }
    }

    private static final class Frame {
        final String type;
        final String method;
        final int depth;
        final String thread;
        final List<String> args;
        final List<Frame> children = new ArrayList<Frame>();
        long durationMs;
        String error = "";
        String kind = "method";
        String sql = "";
        String parameter = "";
        long seq;

        Frame(String type, String method, int depth, String thread, List<String> args) {
            this.type = type;
            this.method = method;
            this.depth = depth;
            this.thread = thread;
            this.args = args;
        }
    }
}
