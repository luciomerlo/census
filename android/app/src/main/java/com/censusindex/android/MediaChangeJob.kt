package com.censusindex.android

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.provider.MediaStore
import kotlin.concurrent.thread

/**
 * Se ejecuta cuando MediaStore cambia, aunque la app no este corriendo.
 * No hay servicio permanente ni sondeo: el sistema nos despierta.
 *
 * Los trabajos con `TriggerContentUri` son de un solo uso: hay que volver a
 * programarlos al empezar (antes de procesar) para no perder cambios.
 */
class MediaChangeJob : JobService() {
    override fun onStartJob(params: JobParameters): Boolean {
        schedule(this)
        thread(name = "censusindex-media-delta") {
            if (Perms.hasAllFiles()) runCatching { Indexer.onMediaChange(applicationContext) }
            jobFinished(params, false)
        }
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean = true

    companion object {
        private const val ID = 4201

        fun schedule(ctx: Context) {
            val js = ctx.getSystemService(JobScheduler::class.java)
            val info = JobInfo.Builder(ID, ComponentName(ctx, MediaChangeJob::class.java))
                .addTriggerContentUri(
                    JobInfo.TriggerContentUri(
                        MediaStore.Files.getContentUri("external"),
                        JobInfo.TriggerContentUri.FLAG_NOTIFY_FOR_DESCENDANTS,
                    )
                )
                // Agrupa rafagas (una foto = varias notificaciones): espera 5 s de
                // calma y como maximo 60 s.
                .setTriggerContentUpdateDelay(5_000)
                .setTriggerContentMaxDelay(60_000)
                .setPersisted(true)
                .build()
            js.schedule(info)
        }
    }
}
