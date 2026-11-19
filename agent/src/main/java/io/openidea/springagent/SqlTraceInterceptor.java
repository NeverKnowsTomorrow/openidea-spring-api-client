package io.openidea.springagent;

import org.apache.ibatis.executor.Executor;
import org.apache.ibatis.mapping.MappedStatement;
import org.apache.ibatis.plugin.Interceptor;
import org.apache.ibatis.plugin.Intercepts;
import org.apache.ibatis.plugin.Invocation;
import org.apache.ibatis.plugin.Plugin;
import org.apache.ibatis.plugin.Signature;

import java.util.Properties;

/** Registered on every MyBatis SqlSessionFactory by the agent, after context startup. */
@Intercepts({
        @Signature(type = Executor.class, method = "query",
                args = {MappedStatement.class, Object.class, org.apache.ibatis.session.RowBounds.class,
                        org.apache.ibatis.session.ResultHandler.class}),
        @Signature(type = Executor.class, method = "update",
                args = {MappedStatement.class, Object.class})
})
public final class SqlTraceInterceptor implements Interceptor {
    @Override
    public Object intercept(Invocation invocation) throws Throwable {
        boolean recordSql = SqlRecorder.isEnabled();
        if (!recordSql && !TraceRecorder.isTracing()) {
            return invocation.proceed();
        }
        long start = System.nanoTime();
        Object[] args = invocation.getArgs();
        String id = "";
        String sql = "";
        String parameter = "";
        try {
            if (args.length > 0 && args[0] instanceof MappedStatement) {
                MappedStatement statement = (MappedStatement) args[0];
                id = statement.getId();
                Object param = args.length > 1 ? args[1] : null;
                parameter = param == null ? "" : String.valueOf(param);
                try {
                    Object bound = statement.getBoundSql(param);
                    Object text = bound.getClass().getMethod("getSql").invoke(bound);
                    sql = text == null ? "" : String.valueOf(text);
                } catch (Throwable ignored) {
                }
            }
            Object result = invocation.proceed();
            long duration = (System.nanoTime() - start) / 1_000_000L;
            if (recordSql) SqlRecorder.record(id, sql, parameter, duration, "");
            TraceRecorder.addSql("MyBatis", sql, parameter, duration, "");
            return result;
        } catch (Throwable error) {
            long duration = (System.nanoTime() - start) / 1_000_000L;
            if (recordSql) SqlRecorder.record(id, sql, parameter, duration, error.toString());
            TraceRecorder.addSql("MyBatis", sql, parameter, duration, error.toString());
            throw error;
        }
    }

    @Override
    public Object plugin(Object target) {
        return Plugin.wrap(target, this);
    }

    @Override
    public void setProperties(Properties properties) {
    }
}
