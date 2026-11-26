package com.censusindex.android

import android.app.Activity
import android.app.AlertDialog
import android.os.Bundle
import android.text.InputType
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Spinner
import android.widget.TextView
import android.widget.Toast
import org.json.JSONArray
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * Nombre del dispositivo y endpoint de IA.
 *
 * Endpoint: nombre, base_url, api_key (vacia en IA local), modo de modelo
 * (auto / elegir de la lista / forzar nombre), respaldo entre modelos y soporte
 * de herramientas. Los presets rellenan la URL; todo sigue editable.
 */
class SettingsActivity : Activity() {
    private lateinit var devName: EditText
    private lateinit var devHint: TextView
    private lateinit var name: EditText
    private lateinit var url: EditText
    private lateinit var key: EditText
    private lateinit var model: EditText
    private lateinit var mode: Spinner
    private lateinit var fallback: CheckBox
    private lateinit var tools: CheckBox
    private lateinit var local: CheckBox
    private val policies = listOf("ask", "always", "never")
    private var polSelected = -1
    private lateinit var polSpinner: Spinner
    private val modes = listOf("auto", "list", "force")
    private val presets = listOf("groq", "ollama", "lmstudio", "llamacpp", "vllm", "custom")
    private val presetUrl = mapOf(
        "groq" to "https://api.groq.com/openai/v1",
        "ollama" to "http://127.0.0.1:11434/v1",
        "lmstudio" to "http://127.0.0.1:1234/v1",
        "llamacpp" to "http://127.0.0.1:8080/v1",
        "vllm" to "http://127.0.0.1:8000/v1",
    )

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        val pad = (16 * resources.displayMetrics.density).toInt()
        val col = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(pad, pad, pad, pad) }
        fun label(t: String) = TextView(this).apply { text = t; textSize = 16f }.also { col.addView(it) }
        fun edit(hint: String, pw: Boolean = false) = EditText(this).apply {
            this.hint = hint
            if (pw) inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            else inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        }.also { col.addView(it, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT)) }

        label("Nombre del dispositivo")
        devName = edit("vacío = nombre del sistema")
        devHint = TextView(this).also { col.addView(it) }
        col.addView(Button(this).apply { text = "Guardar nombre"; setOnClickListener { saveName() } })

        label("\nPendrives, SD y discos USB conectados")
        val pol = Spinner(this).also { col.addView(it) }
        pol.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item,
            listOf("Preguntar cada vez", "Indexar siempre", "No indexar nunca"))
        pol.onItemSelectedListener = object : android.widget.AdapterView.OnItemSelectedListener {
            override fun onItemSelected(p: android.widget.AdapterView<*>?, v: android.view.View?, i: Int, id: Long) {
                if (i == polSelected) return
                polSelected = i
                thread { Indexer.setPolicy(this@SettingsActivity, "*", policies[i]) }
            }
            override fun onNothingSelected(p: android.widget.AdapterView<*>?) {}
        }
        polSpinner = pol
        col.addView(Button(this).apply { text = "Volúmenes conocidos…"; setOnClickListener { knownVolumes() } })

        label("\nIA (lenguaje natural)")
        val preset = Spinner(this).also { col.addView(it) }
        preset.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item, presets)
        name = edit("nombre del endpoint (ej. groq)")
        url = edit("base_url (…/v1, sin /chat/completions)")
        key = edit("api_key (vacía en IA local)", pw = true)
        mode = Spinner(this).also { col.addView(it) }
        mode.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item,
            listOf("Modelo: automático", "Modelo: elegir de la lista", "Modelo: forzar nombre"))
        model = edit("modelo (lista/forzar)")
        fallback = CheckBox(this).apply { text = "Si un modelo falla, probar otro"; isChecked = true }.also { col.addView(it) }
        tools = CheckBox(this).apply { text = "El modelo soporta herramientas (tools)"; isChecked = true }.also { col.addView(it) }
        local = CheckBox(this).apply { text = "IA local (permite http, sin clave)" }.also { col.addView(it) }
        col.addView(Button(this).apply { text = "Ver modelos del endpoint"; setOnClickListener { pickModel() } })
        col.addView(Button(this).apply { text = "Guardar IA"; setOnClickListener { saveAi() } })

        preset.onItemSelectedListener = object : android.widget.AdapterView.OnItemSelectedListener {
            override fun onItemSelected(p: android.widget.AdapterView<*>?, v: android.view.View?, i: Int, id: Long) {
                val k = presets[i]
                presetUrl[k]?.let { url.setText(it); name.setText(k); local.isChecked = k != "groq" }
            }
            override fun onNothingSelected(p: android.widget.AdapterView<*>?) {}
        }
        setContentView(ScrollView(this).apply { addView(col) })
        load()
    }

    private fun load() = thread {
        val d = Indexer.deviceInfo(this)
        val dp = Indexer.policyGet(this).optString("default", "ask")
        val ep = Indexer.aiGet(this).optJSONArray("endpoints")?.optJSONObject(0)
        runOnUiThread {
            polSelected = policies.indexOf(dp).coerceAtLeast(0)
            polSpinner.setSelection(polSelected)
            devName.setText(d.optString("custom"))
            devHint.text = "Nombre actual: ${d.optString("name")}  (sistema: ${d.optString("os_name")})"
            ep?.let {
                name.setText(it.optString("name")); url.setText(it.optString("base_url"))
                key.setText(it.optString("api_key")); model.setText(it.optString("model"))
                mode.setSelection(modes.indexOf(it.optString("mode", "auto")).coerceAtLeast(0))
                fallback.isChecked = it.optBoolean("fallback", true)
                tools.isChecked = it.optBoolean("tools", true)
                local.isChecked = it.optBoolean("local", false)
            }
        }
    }

    private fun saveName() = thread {
        val r = Indexer.setDeviceName(this, devName.text.toString())
        runOnUiThread {
            if (r.has("error")) Toast.makeText(this, r.getString("error"), Toast.LENGTH_LONG).show()
            else {
                devHint.text = "Nombre actual: ${r.optString("name")}  (sistema: ${r.optString("os_name")})"
                Toast.makeText(this, "Guardado; el catálogo lo toma en el próximo reescaneo", Toast.LENGTH_LONG).show()
            }
        }
    }

    private fun endpoint(): JSONObject = JSONObject()
        .put("name", name.text.toString()).put("base_url", url.text.toString())
        .put("api_key", key.text.toString()).put("mode", modes[mode.selectedItemPosition])
        .put("model", model.text.toString()).put("fallback", fallback.isChecked)
        .put("tools", tools.isChecked).put("local", local.isChecked)

    private fun saveAi() = thread {
        val r = Indexer.aiSet(this, JSONObject().put("endpoints", JSONArray().put(endpoint())))
        runOnUiThread {
            Toast.makeText(this, r.optString("error", "IA guardada"), Toast.LENGTH_LONG).show()
        }
    }

    private fun pickModel() = thread {
        val r = Indexer.aiModels(this, endpoint())
        runOnUiThread {
            val arr = r.optJSONArray("models")
            if (arr == null) { Toast.makeText(this, r.optString("error"), Toast.LENGTH_LONG).show(); return@runOnUiThread }
            val items = Array(arr.length()) { arr.getString(it) }
            AlertDialog.Builder(this).setTitle("Modelos (mejor primero)")
                .setItems(items) { _, i -> model.setText(items[i]); mode.setSelection(1) }.show()
        }
    }

    /** Lista de volúmenes vistos; tocar uno rota su política: heredar → siempre → nunca → preguntar. */
    private fun knownVolumes() = thread {
        val p = Indexer.policyGet(this)
        val known = p.optJSONObject("known") ?: JSONObject()
        val per = p.optJSONObject("per_volume") ?: JSONObject()
        val ids = known.keys().asSequence().toList()
        val names = ids.map { id ->
            val k = known.getJSONObject(id)
            "${k.optString("label").ifBlank { k.optString("path") }} — ${per.optString(id, "(global)")}"
        }.toTypedArray()
        runOnUiThread {
            if (ids.isEmpty()) { Toast.makeText(this, "Todavía no se conectó ningún volumen extraíble", Toast.LENGTH_LONG).show(); return@runOnUiThread }
            AlertDialog.Builder(this).setTitle("Tocar para cambiar").setItems(names) { _, i ->
                val cur = per.optString(ids[i], "")
                val next = when (cur) { "" -> "always"; "always" -> "never"; "never" -> "ask"; else -> "always" }
                thread { Indexer.setPolicy(this, ids[i], next) }
                Toast.makeText(this, "→ $next", Toast.LENGTH_SHORT).show()
            }.show()
        }
    }
}
