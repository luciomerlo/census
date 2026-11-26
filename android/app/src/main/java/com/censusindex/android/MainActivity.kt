package com.censusindex.android

import android.app.Activity
import android.os.Bundle
import android.text.Editable
import android.text.TextWatcher
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.Switch
import android.widget.TextView
import kotlin.concurrent.thread

class MainActivity : Activity() {
    private lateinit var status: TextView
    private lateinit var list: ListView
    private lateinit var ai: Switch
    private val rows = ArrayList<String>()
    private lateinit var adapter: ArrayAdapter<String>

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        val pad = (16 * resources.displayMetrics.density).toInt()
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(pad, pad, pad, pad) }
        status = TextView(this)
        val perm = Button(this).apply {
            text = "Permitir acceso a todos los archivos (solo lectura)"
            setOnClickListener { startActivity(Perms.requestIntent(this@MainActivity)) }
        }
        val reindex = Button(this).apply {
            text = "Indexar ahora"
            setOnClickListener { run { Indexer.full(this@MainActivity) } }
        }
        val settings = Button(this).apply {
            text = "Nombre e IA…"
            setOnClickListener { startActivity(android.content.Intent(this@MainActivity, SettingsActivity::class.java)) }
        }
        ai = Switch(this).apply { text = "IA (lenguaje natural)" }
        val serve = Switch(this).apply {
            text = "Compartir índice en la red/Tailscale (puerto ${ServeService.PORT})"
            setOnCheckedChangeListener { _, on -> if (on) ServeService.start(context) else ServeService.stop(context) }
        }
        val q = EditText(this).apply { hint = "Buscar (path: ext: size:>)" }
        adapter = ArrayAdapter(this, android.R.layout.simple_list_item_1, rows)
        list = ListView(this).apply { adapter = this@MainActivity.adapter }
        q.addTextChangedListener(object : TextWatcher {
            override fun afterTextChanged(s: Editable?) { search(s.toString()) }
            override fun beforeTextChanged(s: CharSequence?, a: Int, c: Int, d: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, c: Int, d: Int) {}
        })
        for (v in listOf(status, perm, reindex, settings, serve, ai, q)) root.addView(v, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
        root.addView(list, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        setContentView(root)
    }

    private val volumeCb = object : android.os.storage.StorageManager.StorageVolumeCallback() {
        override fun onStateChanged(volume: android.os.storage.StorageVolume) {
            // Pendrive conectado/desconectado mientras la app esta abierta.
            thread { if (Perms.hasAllFiles()) Indexer.onVolumesChanged(this@MainActivity); refreshStatus() }
        }
    }

    override fun onPause() {
        super.onPause()
        getSystemService(android.os.storage.StorageManager::class.java).unregisterStorageVolumeCallback(volumeCb)
    }

    override fun onResume() {
        super.onResume()
        getSystemService(android.os.storage.StorageManager::class.java)
            .registerStorageVolumeCallback(mainExecutor, volumeCb)
        // Android 13+: sin este permiso la pregunta por un pendrive no se ve.
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        if (Perms.hasAllFiles()) {
            MediaChangeJob.schedule(this)
            Indexer.schedulePeriodic(this)
            thread {
                val s = Indexer.stats(this)
                if (s.optLong("nodes") == 0L) Indexer.full(this) else Indexer.onMediaChange(this)
                refreshStatus()
            }
        } else {
            status.text = "Falta el permiso de acceso a todos los archivos."
        }
    }

    private fun refreshStatus() {
        val s = Indexer.stats(this)
        runOnUiThread {
            status.text = "${s.optLong("nodes")} entradas · ${s.optInt("volumes")} volúmenes · ${s.optString("marca")}\n" +
                "token API: ${Indexer.token(this)}"
        }
    }

    private fun run(block: () -> Unit) = thread { block(); refreshStatus() }

    private fun search(q: String) = thread {
        if (ai.isChecked) {
            // La IA se consulta con Enter implícito al dejar de tipear: se evita una
            // llamada de red por letra exigiendo al menos 3 palabras.
            if (q.trim().split(Regex("\\s+")).size < 3) return@thread
            val r = Indexer.ask(this, q)
            val text = r.optString("text", r.optString("error"))
            runOnUiThread { rows.clear(); rows.addAll(text.lines()); adapter.notifyDataSetChanged() }
            return@thread
        }
        val r = Indexer.search(this, q, 200).optJSONArray("results")
        val out = ArrayList<String>()
        if (r != null) for (i in 0 until r.length()) out.add(r.getJSONObject(i).getString("path"))
        runOnUiThread { rows.clear(); rows.addAll(out); adapter.notifyDataSetChanged() }
    }
}
