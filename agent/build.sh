#!/bin/sh
set -eu
cd "$(dirname "$0")"
GRADLE="$HOME/.gradle/caches/modules-2/files-2.1"
BOOT=$(find "$GRADLE" -name 'spring-boot-2.7.18.jar' | head -1)
CONTEXT=$(find "$GRADLE" -name 'spring-context-5.3.39.jar' | head -1)
CORE=$(find "$GRADLE" -name 'spring-core-5.3.39.jar' | head -1)
BEANS=$(find "$GRADLE" -name 'spring-beans-5.3.39.jar' | head -1)
JACKSON_DB=$(find "$HOME/.m2/repository" -name 'jackson-databind-2.11.0.jar' | head -1)
JACKSON_CORE=$(find "$HOME/.m2/repository" -name 'jackson-core-2.11.0.jar' | head -1)
JACKSON_ANN=$(find "$HOME/.m2/repository" -name 'jackson-annotations-2.11.0.jar' | head -1)
BYTE_BUDDY=$(find "$HOME/.gradle/caches" -name 'byte-buddy-1.12.9.jar' | head -1)
MYBATIS=$(find "$HOME/.m2/repository" -name 'mybatis-3.5.7.jar' | head -1)
if [ -z "$BOOT" ] || [ -z "$CONTEXT" ] || [ -z "$CORE" ] || [ -z "$BEANS" ] || [ -z "$JACKSON_DB" ] || [ -z "$BYTE_BUDDY" ] || [ -z "$MYBATIS" ]; then
  echo "Spring Boot 2.7 compile jars not found in $GRADLE" >&2
  exit 1
fi
rm -rf build/classes
mkdir -p build/classes
javac --release 8 -encoding UTF-8 -cp "$BOOT:$CONTEXT:$CORE:$BEANS:$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN:$BYTE_BUDDY:$MYBATIS" -d build/classes $(find src/main/java -name '*.java')
(cd build/classes && jar xf "$BYTE_BUDDY")
rm -rf build/classes/META-INF
cp -R src/main/resources/. build/classes/
jar cfm openidea-spring-agent.jar build/manifest.mf -C build/classes .
