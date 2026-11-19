package io.openidea.springagent;

import net.bytebuddy.agent.builder.AgentBuilder;
import net.bytebuddy.agent.builder.ResettableClassFileTransformer;
import net.bytebuddy.asm.Advice;
import net.bytebuddy.description.type.TypeDescription;
import net.bytebuddy.dynamic.DynamicType;
import net.bytebuddy.utility.JavaModule;

import java.lang.instrument.Instrumentation;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/** Installs method-level trace hooks; only the controller methods the user selected are transformed. */
public final class TraceInstaller {
    private static final List<ResettableClassFileTransformer> TRANSFORMERS =
            new ArrayList<ResettableClassFileTransformer>();

    private TraceInstaller() {
    }

    /** Replaces the current hooks with exactly these target methods. */
    static synchronized void configure(Instrumentation instrumentation, Object context, List<Target> targets) {
        if (instrumentation == null) {
            return;
        }
        reset(instrumentation);
        if (context == null || targets == null || targets.isEmpty()) {
            return;
        }
        ClassLoader loader = context.getClass().getClassLoader();
        Set<String> seen = new LinkedHashSet<String>();
        for (Target target : targets) {
            if (target == null || target.type == null || target.type.isEmpty()
                    || target.method == null || target.method.isEmpty()) {
                continue;
            }
            String key = target.type + "#" + target.method;
            if (!seen.add(key)) {
                continue;
            }
            try {
                Class<?> type = Class.forName(target.type, false, loader);
                if (type.isInterface() || type.isArray() || type.isPrimitive() || type.isEnum()) {
                    continue;
                }
                final String method = target.method;
                ResettableClassFileTransformer transformer = new AgentBuilder.Default()
                        .with(AgentBuilder.RedefinitionStrategy.RETRANSFORMATION)
                        .disableClassFormatChanges()
                        .type(net.bytebuddy.matcher.ElementMatchers.named(target.type))
                        .transform(new AgentBuilder.Transformer() {
                            @Override
                            public DynamicType.Builder<?> transform(DynamicType.Builder<?> builder,
                                                                    TypeDescription typeDescription,
                                                                    ClassLoader classLoader,
                                                                    JavaModule module) {
                                return builder.visit(Advice.to(TraceAdvice.class)
                                        .on(net.bytebuddy.matcher.ElementMatchers.named(method)));
                            }
                        })
                        .installOn(instrumentation);
                TRANSFORMERS.add(transformer);
            } catch (Throwable ignored) {
            }
        }
    }

    private static void reset(Instrumentation instrumentation) {
        for (ResettableClassFileTransformer transformer : TRANSFORMERS) {
            try {
                transformer.reset(instrumentation, AgentBuilder.RedefinitionStrategy.RETRANSFORMATION);
            } catch (Throwable ignored) {
            }
        }
        TRANSFORMERS.clear();
    }

    static final class Target {
        final String type;
        final String method;

        Target(String type, String method) {
            this.type = type;
            this.method = method;
        }
    }
}
