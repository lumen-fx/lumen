package dev.lumenfx.lumen

import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.ProjectRootManager
import com.intellij.openapi.util.SystemInfo
import java.io.File
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

/**
 * Finds the `lumen-lsp` binary the same way the VS Code extension does: an
 * explicit setting wins, then a locally built binary under the project's Cargo
 * target directories, then the one an installed toolchain ships beside
 * `lumenc`, then `PATH`.
 */
object LumenLspBinary {

    const val NAME: String = "lumen-lsp"

    /**
     * @param command the command to spawn, absolute when a file was found.
     * @param found whether a real file backs the command.
     */
    data class Resolved(val command: String, val found: Boolean)

    fun resolve(project: Project): Resolved {
        val settings = LumenSettings.getInstance(project).state

        val explicit = expandHome(settings.serverPath.trim())
        if (explicit.isNotEmpty()) {
            return Resolved(explicit, Files.isRegularFile(Paths.get(explicit)))
        }

        if (settings.autoDiscover) {
            for (candidate in targetCandidates(project) + installCandidates()) {
                if (Files.isRegularFile(candidate)) {
                    return Resolved(candidate.toString(), true)
                }
            }
        }

        val onPath = findOnPath(NAME)
        return if (onPath != null) Resolved(onPath.toString(), true) else Resolved(NAME, false)
    }

    /**
     * Where an installed toolchain keeps the server: beside `lumenc` on `PATH`
     * (every toolchain archive ships the two together), then the default
     * prefixes of `install.sh` (`~/.lumen/bin`) and the Windows installer
     * (`%LOCALAPPDATA%\Programs\Lumen\bin`). An IDE started from a desktop
     * launcher often lacks the shell's `PATH`, which is why the prefixes are
     * probed by name.
     */
    private fun installCandidates(): List<Path> {
        val dirs = ArrayList<Path>()
        findOnPath("lumenc")?.let { lumenc ->
            val real = runCatching { lumenc.toRealPath() }.getOrDefault(lumenc)
            real.parent?.let { dirs.add(it) }
        }
        dirs.add(Paths.get(System.getProperty("user.home"), ".lumen", "bin"))
        if (SystemInfo.isWindows) {
            System.getenv("LOCALAPPDATA")?.takeIf { it.isNotBlank() }?.let {
                dirs.add(Paths.get(it, "Programs", "Lumen", "bin"))
            }
        }
        val binary = executableName(NAME)
        return dirs.map { it.resolve(binary) }
    }

    /** The first executable named [name] in a `PATH` directory. */
    private fun findOnPath(name: String): Path? {
        val path = System.getenv("PATH") ?: return null
        val binary = executableName(name)
        for (directory in path.split(File.pathSeparatorChar)) {
            if (directory.isBlank()) {
                continue
            }
            val candidate = Paths.get(directory, binary)
            if (Files.isRegularFile(candidate) && Files.isExecutable(candidate)) {
                return candidate.toAbsolutePath().normalize()
            }
        }
        return null
    }

    private fun executableName(name: String): String = if (SystemInfo.isWindows) "$name.exe" else name

    /** `target/{release,debug}/lumen-lsp` under every root the project knows about. */
    private fun targetCandidates(project: Project): List<Path> {
        val roots = LinkedHashSet<Path>()

        System.getenv("CARGO_TARGET_DIR")?.takeIf { it.isNotBlank() }?.let { roots.add(Paths.get(it)) }
        project.basePath?.let { roots.add(Paths.get(it, "target")) }
        for (contentRoot in ProjectRootManager.getInstance(project).contentRoots) {
            contentRoot.canonicalPath?.let { roots.add(Paths.get(it, "target")) }
        }

        val binary = executableName(NAME)
        // Release first: that is the build the docs tell you to make.
        return roots.flatMap { listOf(it.resolve("release").resolve(binary), it.resolve("debug").resolve(binary)) }
    }

    private fun expandHome(path: String): String = when {
        path == "~" -> System.getProperty("user.home")
        path.startsWith("~/") || path.startsWith("~\\") ->
            Paths.get(System.getProperty("user.home"), path.substring(2)).toString()
        else -> path
    }
}
