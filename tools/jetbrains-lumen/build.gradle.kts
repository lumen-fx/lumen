plugins {
    kotlin("jvm") version "2.1.21"
    id("org.jetbrains.intellij.platform") version "2.18.1"
}

group = providers.gradleProperty("pluginGroup").get()
version = providers.gradleProperty("pluginVersion").get()

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    intellijPlatform {
        create(
            providers.gradleProperty("platformType"),
            providers.gradleProperty("platformVersion"),
        )
        bundledPlugin("org.jetbrains.plugins.textmate")
        plugin("com.redhat.devtools.lsp4ij", providers.gradleProperty("lsp4ijVersion").get())
        pluginVerifier()
    }
}

kotlin {
    jvmToolchain(21)
}

intellijPlatform {
    // One settings page, so indexing it is not worth the headless IDE run.
    buildSearchableOptions = false

    pluginConfiguration {
        name = providers.gradleProperty("pluginName")
        version = providers.gradleProperty("pluginVersion")
        ideaVersion {
            sinceBuild = providers.gradleProperty("pluginSinceBuild")
            untilBuild = provider { null }
        }
    }

    pluginVerification {
        ides {
            recommended()
        }
    }

    publishing {
        token = providers.environmentVariable("PUBLISH_TOKEN")
    }
}

// The TextMate grammars and the editor configuration come from the VS Code
// extension, so both editors highlight Lumen the same way and there is one
// place to fix a grammar.
val vscodeExtension = layout.projectDirectory.dir("../vscode-lumen")

tasks.processResources {
    from(vscodeExtension.dir("syntaxes")) {
        into("textmate/syntaxes")
        include("lumen.tmLanguage.json", "lumen-css.tmLanguage.json", "rhai.tmLanguage.json")
    }
    from(vscodeExtension.dir("language-configuration")) {
        into("textmate/language-configuration")
        include("lumen.json", "lumen-css.json", "rhai.json")
    }
    // Stamp the plugin version into the bundle, so the unpack directory
    // changes with each release.
    val pluginVersion = providers.gradleProperty("pluginVersion").get()
    inputs.property("bundleVersion", pluginVersion)
    filesMatching("textmate/bundle-version.txt") {
        expand("version" to pluginVersion)
    }
}
