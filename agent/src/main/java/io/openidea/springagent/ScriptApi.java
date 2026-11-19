package io.openidea.springagent;

/** Small bridge available to scripts compiled by the agent. */
public final class ScriptApi {
    private ScriptApi() {
    }

    public static Object context() {
        return SpringAgent.contextObject();
    }

    public static Object getBean(String name) {
        Object context = context();
        if (context == null) {
            throw new IllegalStateException("Spring context is not ready");
        }
        try {
            return context.getClass().getMethod("getBean", String.class).invoke(context, name);
        } catch (Exception error) {
            throw new IllegalStateException("Cannot get bean " + name, error);
        }
    }

    public static Object getBean(Class<?> type) {
        Object context = context();
        if (context == null) {
            throw new IllegalStateException("Spring context is not ready");
        }
        try {
            return context.getClass().getMethod("getBean", Class.class).invoke(context, type);
        } catch (Exception error) {
            throw new IllegalStateException("Cannot get bean " + type.getName(), error);
        }
    }
}
