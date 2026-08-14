package dev.lumenfx.lumen

import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import com.redhat.devtools.lsp4ij.AbstractDocumentMatcher

/**
 * A `.css` file is Lumen CSS when it sits in a Lumen app directory, that is,
 * next to a `main.lmn`. Stylesheets elsewhere in the project are left alone.
 */
class LumenCssMatcher : AbstractDocumentMatcher() {
    override fun match(file: VirtualFile, project: Project): Boolean =
        file.parent?.findChild("main.lmn")?.isValid == true
}
