package dev.lumenfx.lumen

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.openapi.project.Project
import com.redhat.devtools.lsp4ij.LanguageServerFactory
import com.redhat.devtools.lsp4ij.server.CannotStartProcessException
import com.redhat.devtools.lsp4ij.server.OSProcessStreamConnectionProvider
import com.redhat.devtools.lsp4ij.server.StreamConnectionProvider

/** Connects LSP4IJ to `lumen-lsp`, which speaks LSP over stdio and takes no arguments. */
class LumenServerFactory : LanguageServerFactory {
    override fun createConnectionProvider(project: Project): StreamConnectionProvider = LumenServer(project)
}

private class LumenServer(project: Project) : OSProcessStreamConnectionProvider() {

    private val resolved = LumenLspBinary.resolve(project)

    init {
        val commandLine = GeneralCommandLine(resolved.command)
        project.basePath?.let { commandLine.withWorkDirectory(it) }
        setCommandLine(commandLine)
    }

    override fun start() {
        if (!resolved.found) {
            throw CannotStartProcessException(
                "Cannot find the Lumen language server. Build it with " +
                    "'cargo build --release -p ${LumenLspBinary.NAME}', then put " +
                    "'${LumenLspBinary.NAME}' on PATH or set its path in " +
                    "Settings | Languages & Frameworks | Lumen.",
            )
        }
        super.start()
    }
}
