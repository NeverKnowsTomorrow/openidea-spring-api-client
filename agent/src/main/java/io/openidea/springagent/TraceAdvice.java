package io.openidea.springagent;

import net.bytebuddy.asm.Advice;

/** Injected into selected methods; never throws into the instrumented method. */
public final class TraceAdvice {
    private TraceAdvice() {
    }

    @Advice.OnMethodEnter
    public static void enter(@Advice.Origin("#t") String type,
                             @Advice.Origin("#m") String method,
                             @Advice.Origin("#d") String descriptor,
                             @Advice.AllArguments Object[] arguments) {
        try {
            TraceRecorder.enter(type, method, descriptor, arguments);
        } catch (Throwable ignored) {
        }
    }

    @Advice.OnMethodExit(onThrowable = Throwable.class)
    public static void exit(@Advice.Thrown Throwable error) {
        try {
            TraceRecorder.exit(error);
        } catch (Throwable ignored) {
        }
    }
}
