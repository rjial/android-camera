cmake_minimum_required(VERSION 3.16)
# Semver from the repository's VERSION file, shared with the PC app and the Android app.
file(READ "${CMAKE_CURRENT_SOURCE_DIR}/../VERSION" ACAM_VERSION)
string(STRIP "${ACAM_VERSION}" ACAM_VERSION)
project(acam-source VERSION ${ACAM_VERSION} LANGUAGES C)

find_package(libobs REQUIRED)
find_package(Threads REQUIRED)

add_library(acam-source MODULE src/acam-source.c)
set_target_properties(acam-source PROPERTIES PREFIX "" C_STANDARD 11)
target_compile_options(acam-source PRIVATE -Wall -Wextra -O2)
target_link_libraries(acam-source PRIVATE OBS::libobs Threads::Threads)
target_compile_definitions(acam-source PRIVATE ACAM_VERSION="${ACAM_VERSION}")

# OBS 30+ on Linux loads user plugins from ~/.config/obs-studio/plugins/<name>/bin/64bit/.
set(ACAM_PLUGIN_DIR "$ENV{HOME}/.config/obs-studio/plugins/acam-source" CACHE PATH "OBS user plugin folder")
install(TARGETS acam-source LIBRARY DESTINATION "${ACAM_PLUGIN_DIR}/bin/64bit")
# Translations: data/locale/<OBS locale>.ini; OBS picks the one matching its UI language.
install(DIRECTORY data/ DESTINATION "${ACAM_PLUGIN_DIR}/data")
