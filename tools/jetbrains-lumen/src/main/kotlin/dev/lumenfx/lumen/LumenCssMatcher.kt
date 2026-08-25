package dev.lumenfx.lumen

import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import com.redhat.devtools.lsp4ij.AbstractDocumentMatcher

/**
 * A `.css` file is Lumen CSS when it sits beside an app's markup: next to a
 * `main.lmn`, or in the `src` of a directory holding `lumen.toml`. The second
 * case covers multi-page apps, whose `src` may name its pages something other
 * than `main.lmn`. Stylesheets elsewhere in the project are left alone.
 */
class LumenCssMatcher : AbstractDocumentMatcher() {
    override fun match(file: VirtualFile, project: Project): Boolean {
        val dir = file.parent ?: return false
        return dir.findChild("main.lmn")?.isValid == true ||
            (dir.name == "src" && dir.parent?.findChild("lumen.toml")?.isValid == true)
    }
}
