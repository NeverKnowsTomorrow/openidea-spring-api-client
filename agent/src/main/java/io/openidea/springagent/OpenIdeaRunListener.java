package io.openidea.springagent;

import org.springframework.boot.SpringApplication;
import org.springframework.boot.SpringApplicationRunListener;
import org.springframework.context.ConfigurableApplicationContext;
import org.springframework.core.env.ConfigurableEnvironment;

/** Captures the Spring Boot context so the agent can expose runtime mappings. */
public final class OpenIdeaRunListener implements SpringApplicationRunListener {
    @SuppressWarnings("unused")
    public OpenIdeaRunListener(SpringApplication application, String[] args) {
    }

    /** Spring Boot 2.3/2.4 callbacks (the 2.7 interface made them default). */
    @SuppressWarnings("unused")
    public void starting() {
    }

    @SuppressWarnings("unused")
    public void environmentPrepared(ConfigurableEnvironment environment) {
    }

    @Override
    public void contextPrepared(ConfigurableApplicationContext context) {
        SpringAgent.contextReady(context);
    }

    @Override
    public void contextLoaded(ConfigurableApplicationContext context) {
        SpringAgent.contextReady(context);
    }

    @Override
    public void started(ConfigurableApplicationContext context) {
        SpringAgent.contextReady(context);
    }

    @Override
    public void running(ConfigurableApplicationContext context) {
        SpringAgent.contextReady(context);
    }

    @Override
    public void failed(ConfigurableApplicationContext context, Throwable exception) {
        if (context != null) SpringAgent.contextReady(context);
    }
}
