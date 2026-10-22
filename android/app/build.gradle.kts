import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Semver from the repository's VERSION file, shared with the PC app and the OBS plugin.
val semver: String = rootProject.file("../VERSION").readText().trim()
val (verMajor, verMinor, verPatch) = Regex("""^(\d+)\.(\d+)\.(\d+)$""").matchEntire(semver)
    ?.destructured?.toList()?.map(String::toInt) ?: error("VERSION must be MAJOR.MINOR.PATCH, got '$semver'")

// Release signing comes from the environment (CI secrets) or android/keystore.properties; without it,
// assembleRelease produces an unsigned APK.
val keystoreProps = Properties().apply {
    rootProject.file("keystore.properties").takeIf { it.exists() }?.inputStream()?.use(::load)
}
fun signingValue(env: String, prop: String): String? = System.getenv(env) ?: keystoreProps.getProperty(prop)

android {
    namespace = "com.acam.camera"
    compileSdk = 35
    buildToolsVersion = "36.1.0"

    defaultConfig {
        applicationId = "com.acam.camera"
        minSdk = 24
        // Kept at 33 to avoid the edge-to-edge and foreground-service changes that come with 34+.
        targetSdk = 33
        // 1.2.3 -> 10203; leaves room for 99 minor and 99 patch releases per major.
        versionCode = verMajor * 10000 + verMinor * 100 + verPatch
        versionName = semver
    }

    signingConfigs {
        val store = signingValue("ACAM_KEYSTORE_FILE", "storeFile")
        if (store != null) {
            create("release") {
                storeFile = file(store)
                storePassword = signingValue("ACAM_KEYSTORE_PASSWORD", "storePassword")
                keyAlias = signingValue("ACAM_KEY_ALIAS", "keyAlias")
                keyPassword = signingValue("ACAM_KEY_PASSWORD", "keyPassword")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.findByName("release")
        }
    }

    buildFeatures {
        compose = true
    }

    // Lists the languages in res/values-* for the per-app language setting (Android 13+).
    androidResources {
        generateLocaleConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.12.01")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
}
