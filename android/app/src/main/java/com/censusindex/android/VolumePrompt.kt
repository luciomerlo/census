package com.censusindex.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * Pregunta que hacer con un volumen extraible recien conectado (pendrive,
 * SD, disco USB). Se analiza el volumen entero, todas las particiones que el
 * sistema monte (cada una llega como un volumen aparte).
 *
 * Respuestas: Indexar (esta vez, y luego se mantiene al dia), Siempre (politica
 * `always` para ese volumen) y Nunca (`never`). Ignorar la notificacion no
 * cambia nada y no se repite hasta la proxima conexion.
 */
object VolumePrompt {
    private const val CH = "volumes"

    fun show(ctx: Context, v: JSONObject) {
        val nm = ctx.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CH, "Volúmenes conectados", NotificationManager.IMPORTANCE_HIGH))
        val id = v.getString("volume_id")
        val gb = v.optLong("total") / 1_000_000_000.0
        val n = Notification.Builder(ctx, CH)
            .setSmallIcon(android.R.drawable.ic_menu_search)
            .setContentTitle("Se conectó ${v.optString("label").ifBlank { v.optString("path") }}")
            .setContentText("%.1f GB · ¿Indexar su contenido en CensusIndex?".format(gb))
            .setAutoCancel(true)
            .addAction(action(ctx, id, "once", "Indexar"))
            .addAction(action(ctx, id, "always", "Siempre"))
            .addAction(action(ctx, id, "never", "Nunca"))
            .build()
        nm.notify(id.hashCode(), n)
    }

    private fun action(ctx: Context, volumeId: String, answer: String, label: String): Notification.Action {
        val i = Intent(ctx, VolumeAnswerReceiver::class.java)
            .putExtra("volume_id", volumeId).putExtra("answer", answer)
        // requestCode distinto por (volumen, respuesta): sin esto Android reutiliza el PendingIntent.
        val pi = PendingIntent.getBroadcast(
            ctx, (volumeId + answer).hashCode(), i, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Action.Builder(null, label, pi).build()
    }
}

class VolumeAnswerReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) {
        val id = intent.getStringExtra("volume_id") ?: return
        val answer = intent.getStringExtra("answer") ?: return
        val pending = goAsync()
        thread(name = "censusindex-volume-answer") {
            try {
                when (answer) {
                    "once" -> Indexer.indexOnce(ctx, id)
                    "always" -> { Indexer.setPolicy(ctx, id, "always"); Indexer.indexPending(ctx) }
                    "never" -> Indexer.setPolicy(ctx, id, "never")
                }
                ctx.getSystemService(NotificationManager::class.java).cancel(id.hashCode())
            } finally { pending.finish() }
        }
    }
}
