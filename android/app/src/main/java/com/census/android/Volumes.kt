package com.census.android

import android.content.Context
import android.os.Environment
import android.os.StatFs
import android.os.storage.StorageManager
import org.json.JSONArray
import org.json.JSONObject

/** Enumera los volumenes montados (interno + SD/USB) para el nucleo Rust. */
object Volumes {
    fun json(ctx: Context): String {
        val sm = ctx.getSystemService(StorageManager::class.java)
        val arr = JSONArray()
        for (v in sm.storageVolumes) {
            if (v.state != Environment.MEDIA_MOUNTED && v.state != Environment.MEDIA_MOUNTED_READ_ONLY) continue
            val dir = v.directory ?: continue
            val st = runCatching { StatFs(dir.path) }.getOrNull()
            arr.put(
                JSONObject()
                    .put("path", dir.path)
                    .put("uuid", v.uuid ?: "")
                    .put("label", v.getDescription(ctx) ?: "")
                    .put("primary", v.isPrimary)
                    .put("removable", v.isRemovable)
                    .put("total", st?.totalBytes ?: 0L)
                    .put("free", st?.availableBytes ?: 0L)
            )
        }
        return arr.toString()
    }
}
