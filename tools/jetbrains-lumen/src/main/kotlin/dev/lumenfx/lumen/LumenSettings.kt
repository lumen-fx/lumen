package dev.lumenfx.lumen

import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project

/** Where to find `lumen-lsp`, stored per project. */
@Service(Service.Level.PROJECT)
@State(name = "LumenSettings", storages = [Storage("lumen.xml")])
class LumenSettings : PersistentStateComponent<LumenSettings.State> {

    class State {
        /** Explicit path to the server binary. Empty means discover it. */
        @JvmField
        var serverPath: String = ""

        /** Probe the project's Cargo target directories before falling back to `PATH`. */
        @JvmField
        var autoDiscover: Boolean = true
    }

    private var state = State()

    override fun getState(): State = state

    override fun loadState(state: State) {
        this.state = state
    }

    companion object {
        fun getInstance(project: Project): LumenSettings = project.service()
    }
}
