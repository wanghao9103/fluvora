plugins {
    id("com.android.application")
}

android {
    namespace = "com.fluvora.demo"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.fluvora.demo"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = rootProject.version.toString()
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(project(":fluvora"))
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.11.0")
}
