package io.openidea.springagent;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.List;

/** Ring buffer of SQL statements executed through MyBatis. */
public final class SqlRecorder {
    private static final int MAX_FRAMES = 500;
    private static final Deque<Frame> FRAMES = new ArrayDeque<Frame>();
    private static volatile boolean enabled;

    private SqlRecorder() {
    }

    public static boolean isEnabled() {
        return enabled;
    }

    public static void setEnabled(boolean value) {
        enabled = value;
    }

    public static void record(String id, String sql, String parameter, long duration, String error) {
        if (!enabled) {
            return;
        }
        Frame frame = new Frame(id, sql, parameter, duration, Thread.currentThread().getName(),
                error == null ? "" : error, System.currentTimeMillis());
        synchronized (FRAMES) {
            FRAMES.addLast(frame);
            while (FRAMES.size() > MAX_FRAMES) {
                FRAMES.removeFirst();
            }
        }
    }

    public static String framesJson() {
        List<Frame> copy;
        synchronized (FRAMES) {
            copy = new ArrayList<Frame>(FRAMES);
        }
        StringBuilder out = new StringBuilder("{\"enabled\":").append(enabled).append(",\"frames\":[");
        for (int i = copy.size() - 1; i >= 0; i--) {
            if (i != copy.size() - 1) {
                out.append(',');
            }
            Frame frame = copy.get(i);
            out.append("{\"id\":\"").append(escape(frame.id))
                    .append("\",\"sql\":\"").append(escape(frame.sql))
                    .append("\",\"parameter\":\"").append(escape(frame.parameter))
                    .append("\",\"durationMs\":").append(frame.duration)
                    .append(",\"thread\":\"").append(escape(frame.thread))
                    .append("\",\"error\":\"").append(escape(frame.error))
                    .append("\",\"at\":").append(frame.at).append('}');
        }
        return out.append("]}").toString();
    }

    private static String escape(String value) {
        if (value == null) {
            return "";
        }
        String text = value.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n").replace("\r", "");
        return text.length() > 2000 ? text.substring(0, 2000) + "…" : text;
    }

    private static final class Frame {
        final String id;
        final String sql;
        final String parameter;
        final long duration;
        final String thread;
        final String error;
        final long at;

        Frame(String id, String sql, String parameter, long duration, String thread, String error, long at) {
            this.id = id;
            this.sql = sql;
            this.parameter = parameter;
            this.duration = duration;
            this.thread = thread;
            this.error = error;
            this.at = at;
        }
    }
}
