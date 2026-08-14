package dev.lumenfx.lumen

import com.intellij.openapi.options.Configurable
import com.intellij.openapi.project.Project
import com.intellij.ui.components.JBCheckBox
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBTextField
import com.intellij.util.ui.FormBuilder
import com.redhat.devtools.lsp4ij.LanguageServerManager
import javax.swing.JComponent
import javax.swing.JPanel

/** Settings | Languages &amp; Frameworks | Lumen. */
class LumenConfigurable(private val project: Project) : Configurable {

    private val serverPath = JBTextField()
    private val autoDiscover = JBCheckBox(
        "Look for a locally built server under target/ before falling back to PATH",
    )

    override fun getDisplayName(): String = "Lumen"

    override fun createComponent(): JComponent {
        reset()
        return FormBuilder.createFormBuilder()
            .addLabeledComponent(JBLabel("Path to ${LumenLspBinary.NAME}:"), serverPath, 1, false)
            .addComponent(autoDiscover, 1)
            .addComponentFillVertically(JPanel(), 0)
            .panel
    }

    override fun isModified(): Boolean {
        val state = LumenSettings.getInstance(project).state
        return serverPath.text.trim() != state.serverPath || autoDiscover.isSelected != state.autoDiscover
    }

    override fun apply() {
        val state = LumenSettings.getInstance(project).state
        state.serverPath = serverPath.text.trim()
        state.autoDiscover = autoDiscover.isSelected
        restartServer()
    }

    override fun reset() {
        val state = LumenSettings.getInstance(project).state
        serverPath.text = state.serverPath
        autoDiscover.isSelected = state.autoDiscover
    }

    /** Pick up the new binary without asking the user to restart the IDE. */
    private fun restartServer() {
        val manager = LanguageServerManager.getInstance(project)
        manager.stop(SERVER_ID, LanguageServerManager.StopOptions().setWillDisable(false))
        manager.start(SERVER_ID)
    }

    private companion object {
        const val SERVER_ID = "lumen"
    }
}
