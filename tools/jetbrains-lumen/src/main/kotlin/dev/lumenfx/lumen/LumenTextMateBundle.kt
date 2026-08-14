package dev.lumenfx.lumen

import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.application.PathManager
import com.intellij.openapi.diagnostic.logger
import com.intellij.openapi.extensions.PluginId
import org.jetbrains.plugins.textmate.api.TextMateBundleProvider
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

/**
 * Hands the IDE the Lumen TextMate grammars, which is what colors `.lmn` and
 * `.rhai` files. The bundle ships inside the plugin jar and is unpacked once
 * per plugin version, because TextMate reads bundles from a directory.
 */
class LumenTextMateBundle : TextMateBundleProvider {

    override fun getBundles(): List<TextMateBundleProvider.PluginBundle> {
        val directory = unpack() ?: return emptyList()
        return listOf(TextMateBundleProvider.PluginBundle("Lumen", directory))
    }

    private fun unpack(): Path? {
        val version = PluginManagerCore.getPlugin(PluginId.getId(PLUGIN_ID))?.version ?: "dev"
        val directory = Path.of(PathManager.getSystemPath(), "lumen-textmate", version)
        return try {
            for (file in BUNDLE_FILES) {
                val target = directory.resolve(file)
                if (Files.exists(target)) {
                    continue
                }
                Files.createDirectories(target.parent)
                val resource = LumenTextMateBundle::class.java.getResourceAsStream("$RESOURCES/$file")
                    ?: error("missing bundle resource $file")
                resource.use { Files.copy(it, target, StandardCopyOption.REPLACE_EXISTING) }
            }
            directory
        } catch (e: Exception) {
            LOG.error("Lumen: cannot unpack the TextMate bundle to $directory", e)
            null
        }
    }

    private companion object {
        const val PLUGIN_ID = "dev.lumenfx.lumen"
        const val RESOURCES = "/textmate"

        /** Every file the bundle needs, including the paths named in package.json. */
        val BUNDLE_FILES = listOf(
            "package.json",
            "syntaxes/lumen.tmLanguage.json",
            "syntaxes/lumen-css.tmLanguage.json",
            "syntaxes/rhai.tmLanguage.json",
            "language-configuration/lumen.json",
            "language-configuration/lumen-css.json",
            "language-configuration/rhai.json",
        )

        val LOG = logger<LumenTextMateBundle>()
    }
}
