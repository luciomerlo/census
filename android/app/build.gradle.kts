plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.census.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.census.android"
        // 30: StorageVolume.directory, MediaStore.getGeneration y GENERATION_MODIFIED.
        minSdk = 30
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += listOf("arm64-v8a") }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    // libcensus_android.so llega de scripts/build-android.sh (cargo-ndk).
    sourceSets["main"].jniLibs.srcDir("src/main/jniLibs")
}

dependencies {
    implementation("androidx.work:work-runtime-ktx:2.9.1")
}
